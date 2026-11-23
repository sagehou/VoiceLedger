using System.Collections.Concurrent;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;
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
        string scratch = Path.Combine(Path.GetTempPath(), "VoiceLedger-synthetic-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(scratch);
        try
        {
            await VerifyBothAsync(scratch);
            await VerifySilentLoopbackAsync(scratch);
            await VerifyIdleSealsAsync(scratch);
            await VerifySuspendAsync(scratch);
            Console.WriteLine("Windows capture smoke passed synthetic packet positions, loopback silence, idle sealing, independent tracks, endpoint loss, pause, reconnect, format changes, and suspend.");
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

    private static async Task VerifyBothAsync(string scratch)
    {
        var factory = new ScriptedFactory();
        var mic = Endpoint(CaptureSource.Mic);
        var system = Endpoint(CaptureSource.System);
        await using RecordingSession session = await RecordingSession.StartAsync(scratch, [mic, system], factory.Open);
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

        VerifyStream(session.OutputDirectory, micTrack, 11, qpc, expectedGap: true);
        VerifyStream(session.OutputDirectory, systemTrack, 22, qpc, expectedGap: false);
    }

    private static async Task VerifySilentLoopbackAsync(string scratch)
    {
        var factory = new ScriptedFactory();
        await using RecordingSession session = await RecordingSession.StartAsync(scratch, [Endpoint(CaptureSource.System)], factory.Open);
        await session.StopAsync();
        string directory = TrackDirectory(session.OutputDirectory, session.Tracks.Single());
        Expect(Directory.GetFiles(directory, "*.wav").Length == 0, "idle loopback writes no fake audio frames");
        var events = ReadEvents(directory);
        Expect(events.Any(row => Kind(row) == "silence") && events.All(row => Kind(row) != "gap"),
            "elapsed loopback without callbacks is silence, not a disconnect");
    }

    private static async Task VerifySuspendAsync(string scratch)
    {
        var factory = new ScriptedFactory();
        var mic = Endpoint(CaptureSource.Mic);
        await using RecordingSession session = await RecordingSession.StartAsync(scratch, [mic], factory.Open);
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

    private static async Task VerifyIdleSealsAsync(string scratch)
    {
        var factory = new ScriptedFactory();
        var system = Endpoint(CaptureSource.System);
        await using RecordingSession session = await RecordingSession.StartAsync(scratch, [system], factory.Open);
        var track = session.Tracks.Single();
        factory.Latest(system.Id).Push(42, 0, Qpc100Ns());
        await UntilAsync(() => track.Files.SealedSegments == 1, "loopback idle seals existing audio while recording stays active");
        Expect(session.State == SessionState.Recording && track.Run?.Completion.IsCompleted == false,
            "idle checkpoint keeps capture running");
    }

    private static void VerifyStream(string output, RecordingTrack track, byte first, long qpc, bool expectedGap)
    {
        string directory = TrackDirectory(output, track);
        var events = ReadEvents(directory);
        JsonElement[] segments = events.Where(row => Kind(row) == "wav_segment").ToArray();
        Expect(segments.Length > 0, "real test PCM reached a sealed segment");
        long sequence = -1;
        foreach (JsonElement segment in segments)
        {
            long current = segment.GetProperty("sequence").GetInt64();
            Expect(current > sequence, "segment sequence remains monotonic across stream restarts");
            sequence = current;
            string filename = Path.Combine(directory, segment.GetProperty("filename").GetString()!);
            using var file = File.OpenRead(filename);
            Expect(Convert.ToHexStringLower(SHA256.HashData(file)) == segment.GetProperty("sha256").GetString(), "sealed WAV bytes match journal checksum");
        }
        JsonElement packet = segments[0].GetProperty("packets")[0];
        Expect(packet.GetProperty("qpc100ns").GetInt64() == qpc && packet.GetProperty("device_position_frames").GetInt64() == 0,
            "native packet device and QPC positions are preserved");
        using (var wave = new WaveFileReader(Path.Combine(directory, segments[0].GetProperty("filename").GetString()!)))
        {
            var pcm = new byte[wave.WaveFormat.BlockAlign];
            Expect(wave.Read(pcm, 0, pcm.Length) == pcm.Length && pcm[0] == first,
                "each WAV stores its own track's original frame bytes");
        }
        bool hasEndpointGap = events.Any(row => Kind(row) == "gap" && row.GetProperty("cause").GetString() == "endpoint_lost");
        Expect(hasEndpointGap == expectedGap, "endpoint loss marks only the affected track");
    }

    private static string TrackDirectory(string output, RecordingTrack track) =>
        Directory.GetDirectories(output).Single(path => path.EndsWith(track.Files.Id.ToString("D"), StringComparison.Ordinal));

    private static JsonElement[] ReadEvents(string directory) => File.ReadLines(Path.Combine(directory, "timeline.jsonl"))
        .Select(row => JsonSerializer.Deserialize<JsonElement>(row)).ToArray();

    private static string? Kind(JsonElement record) => record.GetProperty("kind").GetString();

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
