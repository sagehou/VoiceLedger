using System.Collections.Concurrent;
using System.Diagnostics;
using System.Runtime.InteropServices;
using NAudio.CoreAudioApi;
using NAudio.Wave;
using VoiceLedger;
using VoiceLedger.Capture;
using VoiceLedger.Core;

internal static class RecordingSmoke
{
    internal static async Task RunAsync()
    {
        await VerifyEndpointDiscoveryAsync();
        VerifyTimeline();
        VerifyResampling();
        string scratch = Path.Combine(Path.GetTempPath(), "VoiceLedger-synthetic-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(scratch);
        try
        {
            using (var store = new AudioStore(scratch))
            {
                await VerifyBothAsync(store);
                await VerifySilentLoopbackAsync(store);
                await VerifyIdleSealsAsync(store);
                await VerifySuspendAsync(store);
            }
            using var reopened = new AudioStore(scratch);
            Expect(reopened.History().Count(track => track.CommittedChunks > 0) >= 3
                && reopened.Recovery().MissingChunks == 0 && reopened.Recovery().CorruptChunks == 0,
                "reopened Windows store keeps committed track history");
            Console.WriteLine("Windows capture smoke passed Opus persistence, seek, loopback silence, idle sealing, independent tracks, endpoint loss, pause, reconnect, format changes, and suspend.");
        }
        finally
        {
            Directory.Delete(scratch, recursive: true);
        }
    }

    private static async Task VerifyEndpointDiscoveryAsync()
    {
        Expect(EndpointCatalog.ReadOptionalText(() => throw new UnauthorizedAccessException()) is null,
            "an unreadable device property allows a display fallback");
        Expect(EndpointCatalog.ReadOptionalText(() => throw new NotImplementedException()) is null,
            "an unsupported device property allows a display fallback");
        Expect(EndpointCatalog.ReadOptionalText(() => "Test endpoint") == "Test endpoint",
            "readable endpoint metadata is preserved");

        await using var viewModel = new RecordingViewModel();
        await viewModel.RefreshDevicesAsync(initial: true);
        Expect(viewModel.Status is "Ready" or "No active audio endpoints found.",
            $"Windows audio discovery returned {viewModel.Status}");
        Console.WriteLine($"Windows audio discovery completed with {viewModel.Microphones.Count} capture and " +
            $"{viewModel.RenderEndpoints.Count} render endpoints on the hosted runner.");
    }

    private static void VerifyTimeline()
    {
        var clock = new CaptureClock();
        Expect(clock.MapQpc(clock.OriginQpc100ns + 10_000, 1_100_000) == 1_000_000, "WASAPI timestamps use 100 ns units");
        var system = new PacketTimeline(CaptureSource.System, 48000, 0);
        var mic = new PacketTimeline(CaptureSource.Mic, 48000, 0);
        var first = new PacketStamp(480, 0, 0, AudioClientBufferFlags.None);
        system.Place(first, 0, 15_000_000);
        mic.Place(first, 0, 15_000_000);
        var next = new PacketStamp(480, 4800, 0, AudioClientBufferFlags.None);
        PlacedPacket render = system.Place(next, 100_000_000, 115_000_000);
        PlacedPacket capture = mic.Place(next, 100_000_000, 115_000_000);
        Expect(render.StartNs == capture.StartNs, "equal hardware timestamps share one timebase");
        Expect(render.Before is { Kind: "silence", StartNs: 10_000_000, EndNs: 100_000_000 }, "loopback idle is silence");
        Expect(capture.Before is { Kind: "gap", Cause: "source_discontinuity" }, "lost microphone positions are a gap");
        var flagged = next with { Position = 5760, Flags = AudioClientBufferFlags.DataDiscontinuity };
        Expect(system.Place(flagged, 120_000_000, 140_000_000).Before is { Kind: "gap" }, "driver discontinuity is explicit");
        var estimated = flagged with { Flags = AudioClientBufferFlags.TimestampError };
        Expect(system.Place(estimated, 1, 150_000_000).Estimated, "unreliable QPC is marked estimated");
    }

    private static void VerifyResampling()
    {
        var source = new WaveFormat(44100, 16, 2);
        byte[] bytes = Enumerable.Repeat((byte)16, 441 * source.BlockAlign).ToArray();
        float[] samples = ChunkPcm.AtPlaybackRate(source, bytes);
        Expect(samples.Length == 480 * 2 && samples.Any(value => value > 0.05f),
            "44.1 kHz stereo reaches the exact authored Opus frame count");
    }

    private static async Task VerifyBothAsync(AudioStore store)
    {
        var factory = new ScriptedFactory();
        var mic = Endpoint(CaptureSource.Mic);
        var system = Endpoint(CaptureSource.System);
        await using RecordingSession session = await RecordingSession.StartAsync(store, [mic, system], factory.Open);
        Expect(session.State == SessionState.Recording, "both selected endpoints start");
        var micTrack = session.Tracks.Single(track => track.Files.Endpoint.Source == CaptureSource.Mic);
        var systemTrack = session.Tracks.Single(track => track.Files.Endpoint.Source == CaptureSource.System);
        ScriptedInput oldMic = factory.Latest(mic.Id);
        ScriptedInput render = factory.Latest(system.Id);
        long qpc = Qpc100Ns();
        oldMic.Push(11, 0, qpc);
        render.Push(22, 0, qpc);
        await UntilAsync(() => oldMic.Reads == 1 && render.Reads == 1, "both sources accepted their first packet");
        oldMic.ChangeDefault();
        await session.RefreshAsync();
        Expect(micTrack.Core.State == TrackState.Recording && factory.Opened.Count == 2, "default change retains the endpoint");

        oldMic.Disconnect();
        CaptureRun micRun = micTrack.Run!;
        await micRun.Completion.WaitAsync(TimeSpan.FromSeconds(15));
        await session.RefreshAsync();
        Expect(micTrack.Core.State == TrackState.Disconnected && systemTrack.Core.State == TrackState.Recording
            && session.State == SessionState.Recording, "one lost endpoint leaves the other recording");
        render.Push(23, 48, Qpc100Ns(), AudioClientBufferFlags.Silent);
        await UntilAsync(() => render.Reads == 2, "silent system packet reaches writer");
        await session.ReconnectAsync(micTrack.Files.Id);
        Expect(micTrack.Core.Epoch == 1 && systemTrack.Core.Epoch == 0 && factory.Opened.Count == 3,
            "same-endpoint reconnect advances only that source epoch");
        ScriptedInput newMic = factory.Latest(mic.Id);
        newMic.Push(12, 0, Qpc100Ns());
        await UntilAsync(() => newMic.Reads == 1, "reconnected microphone supplies frames");
        await session.PauseAsync();
        Expect(session.State == SessionState.Paused, "pause drains the streams");
        await session.ResumeAsync();
        Expect(session.State == SessionState.Recording && micTrack.Core.Epoch == 1, "resume retains the bound source epoch");
        await session.StopAsync();
        await session.StopAsync();
        Expect(session.State == SessionState.Completed, "stop is idempotent");

        VerifyStream(store, session.OutputDirectory, micTrack, expectedGap: true);
        VerifyStream(store, session.OutputDirectory, systemTrack, expectedGap: false);
    }

    private static async Task VerifySilentLoopbackAsync(AudioStore store)
    {
        var factory = new ScriptedFactory();
        await using RecordingSession session = await RecordingSession.StartAsync(store, [Endpoint(CaptureSource.System)], factory.Open);
        await session.StopAsync();
        var track = session.Tracks.Single();
        Expect(store.History().Single(item => item.Id == track.Files.Id).CommittedChunks == 0, "idle loopback writes no fake audio frames");
        TimelineRow[] events = store.Timeline(track.Files.Id);
        Expect(events.Any(row => row.Kind == "silence") && events.All(row => row.Kind != "gap"),
            "elapsed loopback without callbacks is silence, not a disconnect");
    }

    private static async Task VerifySuspendAsync(AudioStore store)
    {
        var factory = new ScriptedFactory();
        var mic = Endpoint(CaptureSource.Mic);
        await using RecordingSession session = await RecordingSession.StartAsync(store, [mic], factory.Open);
        RecordingTrack track = session.Tracks.Single();
        CaptureRun run = track.Run!;
        session.Suspend();
        await run.Completion.WaitAsync(TimeSpan.FromSeconds(15));
        await session.RefreshAsync();
        Expect(session.State == SessionState.Paused && track.Core.State == TrackState.Disconnected && factory.Opened.Count == 1,
            "suspend pauses the last source without reopening the microphone");
        await session.ReconnectAsync(track.Files.Id);
        Expect(session.State == SessionState.Recording && track.Core.Epoch == 1 && factory.Opened.Count == 2,
            "explicit reconnect resumes the same suspended source");
        var resumed = track.Run!;
        factory.Latest(mic.Id).ChangeFormat();
        await resumed.Completion.WaitAsync(TimeSpan.FromSeconds(15));
        await session.RefreshAsync();
        Expect(session.State == SessionState.Paused && track.LastEnd == CaptureEnd.FormatChanged && factory.Opened.Count == 2,
            "format change stops the old stream and waits for explicit reconnect");
    }

    private static async Task VerifyIdleSealsAsync(AudioStore store)
    {
        var factory = new ScriptedFactory();
        var system = Endpoint(CaptureSource.System);
        await using RecordingSession session = await RecordingSession.StartAsync(store, [system], factory.Open);
        var track = session.Tracks.Single();
        factory.Latest(system.Id).Push(42, 0, Qpc100Ns());
        await UntilAsync(() => track.Files.SealedSegments == 1, "loopback idle seals existing audio while recording stays active");
        Expect(session.State == SessionState.Recording && track.Run?.Completion.IsCompleted == false,
            "idle checkpoint keeps capture running");
        Expect(track.Run?.QueuedPackets is >= 0 and <= CaptureRun.QueuePackets, "bounded writer queue reports its depth");
    }

    private static void VerifyStream(AudioStore store, string output, RecordingTrack track, bool expectedGap)
    {
        string directory = Path.Combine(output, track.Files.Id.ToString("D"));
        TimelineRow[] events = store.Timeline(track.Files.Id);
        TimelineRow[] segments = events.Where(row => row.Kind == "audio").ToArray();
        Expect(segments.Length > 0, "real test PCM reached a sealed segment");
        string[] files = Directory.GetFiles(directory, "*.opus");
        Expect(files.Length == segments.Length && Directory.GetFiles(directory, "*.partial").Length == 0,
            "each committed row resolves to exactly one sealed Opus file");
        foreach (string path in files)
        {
            using var file = File.OpenRead(path);
            var signature = new byte[4];
            file.ReadExactly(signature);
            Expect(signature.AsSpan().SequenceEqual("OggS"u8), "the sealed media uses Ogg Opus");
        }
        TimelineRow first = segments[0];
        PlaybackChunk full = store.Seek(track.Files.Id, first.StartNs);
        PlaybackChunk partial = store.Seek(track.Files.Id, first.StartNs + (first.EndNs - first.StartNs) / 2);
        Expect(full.Channels == 1 && full.NextNs == first.EndNs && partial.Samples.Length > 0
            && partial.Samples.Length < full.Samples.Length, "an in-segment seek decodes committed samples from the selected track");
        Expect(events.Any(row => row.Kind == "gap" && row.Cause == "endpoint_lost") == expectedGap,
            "endpoint loss marks only the affected track");
    }

    private static CaptureEndpoint Endpoint(CaptureSource source) => new($"synthetic-{source}", source.ToString(), source,
        DeviceState.Active, true, "48000 Hz / PCM16");

    private static long Qpc100Ns() => (long)((Int128)Stopwatch.GetTimestamp() * 10_000_000 / Stopwatch.Frequency);

    private static async Task UntilAsync(Func<bool> condition, string contract)
    {
        long deadline = Stopwatch.GetTimestamp() + Stopwatch.Frequency * 15;
        while (!condition() && Stopwatch.GetTimestamp() < deadline) await Task.Delay(10);
        Expect(condition(), contract);
    }

    private static void Expect(bool success, string contract)
    {
        if (!success) throw new InvalidOperationException($"Capture smoke failed: {contract}.");
    }

    private sealed class ScriptedFactory
    {
        internal ConcurrentQueue<(string Id, ScriptedInput Input)> Opened { get; } = new();

        internal ICaptureInput Open(CaptureEndpoint endpoint, Action<CaptureEnd> interrupted)
        {
            var source = new ScriptedInput(endpoint.Id, interrupted);
            Opened.Enqueue((endpoint.Id, source));
            return source;
        }

        internal ScriptedInput Latest(string id) => Opened.Last(entry => entry.Id == id).Input;
    }

    private sealed class ScriptedInput(string endpointId, Action<CaptureEnd> interrupted) : ICaptureInput
    {
        private readonly EndpointWatch watch = new(endpointId, interrupted);
        private readonly AutoResetEvent ready = new(false);
        private readonly ConcurrentQueue<(byte Value, PacketStamp Stamp)> incoming = new();
        private int reads;

        public WaveFormat Format { get; } = new(48000, 16, 1);
        public int BufferFrames => 480;
        public WaitHandle PacketReady => ready;
        internal int Reads => Volatile.Read(ref reads);
        public void Start() { }
        public void Stop() { }
        internal void ChangeDefault() => watch.OnDefaultDeviceChanged(DataFlow.Capture, Role.Multimedia, "synthetic-other-default");
        internal void Disconnect() => watch.OnDeviceRemoved(endpointId);
        internal void ChangeFormat() => watch.OnPropertyValueChanged(endpointId, PropertyKeys.PKEY_AudioEngine_DeviceFormat);

        internal void Push(byte value, long position, long qpc, AudioClientBufferFlags flags = AudioClientBufferFlags.None)
        {
            incoming.Enqueue((value, new PacketStamp(48, position, qpc, flags)));
            ready.Set();
        }

        public bool TryRead(PacketConsumer consume)
        {
            if (!incoming.TryDequeue(out var entry)) return false;
            byte[] sample = Enumerable.Repeat(entry.Value, entry.Stamp.Frames * Format.BlockAlign).ToArray();
            IntPtr memory = Marshal.AllocHGlobal(sample.Length);
            try
            {
                Marshal.Copy(sample, 0, memory, sample.Length);
                consume(memory, entry.Stamp);
                Interlocked.Increment(ref reads);
            }
            finally
            {
                Marshal.FreeHGlobal(memory);
            }
            return true;
        }

        public void Dispose() => ready.Dispose();
    }
}
