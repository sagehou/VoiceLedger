using System.Text.Json;
using VoiceLedger;
using VoiceLedger.Core;

internal static class LiveSpeechSmoke
{
    internal static async Task RunAsync(string executable, string profile, string sample)
    {
        if (!DeviceProfiler.Capture(profile)) throw new InvalidOperationException("Device profile unavailable.");
        string recordings = Path.Combine(profile, "Recordings");
        using var store = new AudioStore(recordings);
        Guid session = Guid.NewGuid();
        Guid track = Guid.NewGuid();
        store.Write(new
        {
            op = "create_session", id = session, createdAtMs = 1, clock = "synthetic", clockOrigin = 0,
            tracks = new[] { new { id = track, source = "mic", endpointId = "synthetic" } },
        });
        store.Write(new
        {
            op = "begin_stream", trackId = track, epoch = 0, startNs = 0, rate = 48000,
            channels = 1, sampleFormat = "pcm_f32le", nativeFormat = "synthetic",
        });
        State(2, 0);

        string Request(object action) => JsonSerializer.Serialize(new
        {
            recordings, profiles = profile,
            adapters = Path.Combine(Path.GetDirectoryName(executable)!, "speech"), action,
        });
        string snapshot = Request(new { op = "live_transcript", session_id = session });
        var tracks = new Dictionary<Guid, string> { [track] = "Microphone" };
        var waiting = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var final = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(75));
        Task<int> worker = SpeechWorker.RunAsync(
            Request(new { op = "transcribe_live", session_id = session, language = "auto", translate = false, approved = true }),
            progress =>
            {
                var report = LiveTranscript.Read(snapshot, progress, tracks);
                if (report.Stage == "waiting_for_audio") waiting.TrySetResult();
                if (report.Lines.Any(line => !line.IsPartial && line.Caption == "Microphone · English" && line.Text.Length > 0))
                    final.TrySetResult();
                return Task.CompletedTask;
            }, cancellation.Token, executable);
        try
        {
            await WaitAsync(waiting.Task, worker, cancellation.Token, "Waiting for saved audio");
            byte[] bytes = File.ReadAllBytes(sample);
            var samples = new float[bytes.Length / sizeof(float)];
            Buffer.BlockCopy(bytes, 0, samples, 0, bytes.Length);
            long end = 0;
            for (int offset = 0; offset < samples.Length; )
            {
                int count = Math.Min(samples.Length - offset, 2 * 48_000);
                long start = end;
                end += count * 1_000_000_000L / 48_000;
                Guid chunk = Guid.NewGuid();
                store.Write(new { op = "begin_chunk", id = chunk, trackId = track, epoch = 0, startNs = start, endNs = end });
                store.Write(new
                {
                    op = "seal_chunk", id = chunk, endNs = end,
                    packets = new[] { new { startNs = start, endNs = end, frames = count,
                        devicePositionFrames = offset, qpc100ns = 0, flags = 0, estimatedTimestamp = false } },
                }, samples[offset..(offset + count)]);
                offset += count;
            }
            await WaitAsync(final.Task, worker, cancellation.Token, "English final while the manifest is recording");
            State(5, end);
            if (await worker.WaitAsync(cancellation.Token) != 0)
                throw new InvalidOperationException("The Windows speech worker did not drain at Stop.");
            Console.WriteLine("Packaged Windows speech worker reported readiness and returned an English final during synthetic capture.");
        }
        finally
        {
            await cancellation.CancelAsync();
            await worker;
        }

        void State(int state, long elapsedNs) =>
            store.Write(new { op = "session_state", id = session, state, elapsedNs, snapshot = new { } });
    }

    private static async Task WaitAsync(Task milestone, Task<int> worker, CancellationToken cancellation, string context)
    {
        if (await Task.WhenAny(milestone, worker).WaitAsync(cancellation) == worker && !milestone.IsCompletedSuccessfully)
            throw new InvalidOperationException($"{context} was missing. Worker exited with code {await worker}.");
    }
}
