using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using VoiceLedger.Core;

namespace VoiceLedger.Capture;

internal sealed class RecordingTrack : IDisposable
{
    internal readonly TrackFiles Files;
    internal readonly CoreTrack Core;
    internal volatile CaptureRun? Run;
    internal PendingGap? Gap;
    internal CaptureEnd? LastEnd;
    internal string? FormatSignature;
    internal bool HadLoss;

    internal RecordingTrack(AudioStore store, CaptureEndpoint endpoint)
    {
        Files = new TrackFiles(store, endpoint);
        Core = new CoreTrack(endpoint.Id);
    }

    public void Dispose() => Core.Dispose();
}

internal sealed class RecordingSession : IAsyncDisposable
{
    private readonly CaptureClock clock = new();
    private readonly CoreSession core = new();
    private readonly AudioStore store;
    private readonly SemaphoreSlim operations = new(1, 1);
    private readonly List<RecordingTrack> tracks = new();
    private readonly List<SessionChange> transitions = new();
    private readonly OpenCaptureInput open;
    private readonly DateTimeOffset created = DateTimeOffset.UtcNow;
    private long endedAtNs;
    private bool disposed;

    private RecordingSession(AudioStore store, IReadOnlyList<CaptureEndpoint> endpoints, OpenCaptureInput open)
    {
        this.store = store;
        this.open = open;
        Id = Guid.NewGuid();
        OutputDirectory = store.SessionDirectory(Id);
        try
        {
            TrackFiles.CheckReserve(store.Root);
            Directory.CreateDirectory(OutputDirectory);
            foreach (CaptureEndpoint endpoint in endpoints)
            {
                tracks.Add(new RecordingTrack(store, endpoint));
            }
            store.Write(new { op = "create_session", id = Id, createdAtMs = created.ToUnixTimeMilliseconds(),
                clock = "windows_qpc", clockOrigin = clock.OriginQpc100ns,
                tracks = tracks.Select(track => new { id = track.Files.Id, endpointId = track.Files.Endpoint.Id,
                    source = track.Files.Endpoint.Source == CaptureSource.Mic ? "mic" : "system_loopback" }).ToArray() });
        }
        catch
        {
            foreach (RecordingTrack track in tracks) track.Dispose();
            core.Dispose();
            operations.Dispose();
            throw;
        }
    }

    internal Guid Id { get; }
    internal string OutputDirectory { get; }
    internal IReadOnlyList<RecordingTrack> Tracks => tracks;
    internal SessionState State { get; private set; } = SessionState.Idle;
    internal bool Active => State is SessionState.Preparing or SessionState.Recording or SessionState.Paused or SessionState.Stopping;
    internal long ElapsedNs => Active ? clock.NowNs : endedAtNs;
    internal bool JournalError { get; private set; }

    internal static async Task<RecordingSession> StartAsync(AudioStore store, IReadOnlyList<CaptureEndpoint> endpoints, OpenCaptureInput? open = null)
    {
        if (endpoints.Count == 0 || endpoints.Any(endpoint => !endpoint.IsAvailable)
            || endpoints.Select(endpoint => endpoint.Source).Distinct().Count() != endpoints.Count)
        {
            throw new InvalidOperationException("Select one available endpoint for each requested source.");
        }

        var session = await Task.Run(() => new RecordingSession(store, endpoints, open ?? WasapiInput.Open));
        try
        {
            session.Move(SessionCommand.Prepare, "user_request");
            await session.SaveSessionAsync(required: true);
            bool[] success = await Task.WhenAll(session.tracks.Select(track => session.OpenTrackAsync(track, TrackCommand.Start)));
            if (success.All(value => value))
            {
                session.Move(SessionCommand.Start, "source_ready");
                await session.SaveSessionAsync();
            }
            else
            {
                await session.FinishAsync(interrupted: true);
            }
            return session;
        }
        catch
        {
            await session.DisposeAsync();
            throw;
        }
    }

    internal Task RefreshAsync() => ExclusiveAsync(async () =>
    {
        bool changed = await CollectEndedTracksAsync();
        if (changed)
        {
            await ReconcileSessionAsync();
        }
    });

    internal Task PauseAsync() => ExclusiveAsync(async () =>
    {
        if (State != SessionState.Recording) return;
        await StopTracksAsync(CaptureEnd.Pause);
        if (tracks.Any(track => track.Core.State is TrackState.Paused or TrackState.Disconnected))
        {
            Move(SessionCommand.Pause, "user_request");
            await SaveSessionAsync();
        }
        else
        {
            await FinishAsync(interrupted: true);
        }
    });

    internal Task ResumeAsync() => ExclusiveAsync(async () =>
    {
        if (State is not (SessionState.Paused or SessionState.Recording)) return;
        await Task.WhenAll(tracks.Where(track => track.Core.State == TrackState.Paused)
            .Select(track => OpenTrackAsync(track, TrackCommand.Resume)));
        if (State == SessionState.Paused && tracks.Any(track => track.Core.State == TrackState.Recording))
        {
            Move(SessionCommand.Resume, "user_request");
        }
        await ReconcileSessionAsync();
    });

    internal Task ReconnectAsync(Guid trackId) => ExclusiveAsync(async () =>
    {
        if (State is not (SessionState.Recording or SessionState.Paused)) return;
        RecordingTrack? track = tracks.SingleOrDefault(item => item.Files.Id == trackId);
        if (track is null || track.Core.State != TrackState.Disconnected) return;
        if (await OpenTrackAsync(track, TrackCommand.Reconnect) && State == SessionState.Paused)
        {
            Move(SessionCommand.Resume, "user_request");
        }
        await ReconcileSessionAsync();
    });

    internal Task StopAsync() => ExclusiveAsync(() => FinishAsync(interrupted: false));

    internal void Suspend()
    {
        foreach (RecordingTrack track in tracks)
        {
            track.Run?.RequestStop(CaptureEnd.SystemSuspend);
        }
    }

    private async Task<bool> OpenTrackAsync(RecordingTrack track, TrackCommand command)
    {
        uint epoch = command == TrackCommand.Reconnect ? checked(track.Core.Epoch + 1) : track.Core.Epoch;
        var run = new CaptureRun(clock, track.Files, epoch, track.Gap, open,
            command == TrackCommand.Resume ? track.FormatSignature : null);
        track.Run = run;
        try
        {
            EpochStart response = await run.Started;
            track.Core.Apply(command, response.AtNs, command == TrackCommand.Reconnect ? track.Files.Endpoint.Id : null);
            track.FormatSignature = CaptureFormat.Signature(response.Format);
            track.Gap = null;
            track.LastEnd = null;
            return true;
        }
        catch (IOException)
        {
            ApplyTrackResult(track, await run.Completion);
            return false;
        }
    }

    private async Task<bool> CollectEndedTracksAsync()
    {
        bool changed = false;
        foreach (RecordingTrack track in tracks)
        {
            CaptureRun? run = track.Run;
            if (run is not null && run.Completion.IsCompleted)
            {
                ApplyTrackResult(track, await run.Completion);
                changed = true;
            }
        }
        return changed;
    }

    private async Task StopTracksAsync(CaptureEnd reason)
    {
        foreach (RecordingTrack track in tracks) track.Run?.RequestStop(reason);
        foreach (RecordingTrack track in tracks)
        {
            CaptureRun? run = track.Run;
            if (run is not null) ApplyTrackResult(track, await run.Completion);
        }
    }

    private static void ApplyTrackResult(RecordingTrack track, EpochResult result)
    {
        track.Run = null;
        track.LastEnd = result.Reason;
        switch (result.Reason)
        {
            case CaptureEnd.Pause:
                track.Core.Apply(TrackCommand.Pause, result.EndNs);
                track.Gap = new PendingGap(track.Core.Epoch, result.EndNs, "paused");
                break;
            case CaptureEnd.Stop:
                track.Core.Apply(TrackCommand.Stop, result.EndNs);
                track.Core.Apply(TrackCommand.Finish, result.EndNs);
                break;
            default:
                track.HadLoss = true;
                if (result.Reason is CaptureEnd.EndpointLost or CaptureEnd.FormatChanged or CaptureEnd.SystemSuspend
                    || track.Core.State == TrackState.Disconnected && !result.Started)
                {
                    if (track.Core.State != TrackState.Disconnected)
                    {
                        track.Core.Apply(TrackCommand.Disconnect, result.EndNs);
                    }
                }
                else
                {
                    track.Core.Apply(TrackCommand.Interrupt, result.EndNs);
                }
                track.Gap ??= new PendingGap(track.Core.Epoch,
                    result.Reason == CaptureEnd.StorageFailure ? result.CoveredUntilNs : result.EndNs,
                    CaptureReason.Code(result.Reason));
                break;
        }
    }

    private async Task ReconcileSessionAsync()
    {
        if (Active && tracks.All(track => track.Core.State != TrackState.Recording))
        {
            if (tracks.Any(track => track.Core.State is TrackState.Disconnected or TrackState.Paused))
            {
                if (State == SessionState.Recording) Move(SessionCommand.Pause, "source_unavailable");
            }
            else
            {
                await FinishAsync(interrupted: true);
                return;
            }
        }
        await SaveSessionAsync();
    }

    private async Task FinishAsync(bool interrupted)
    {
        if (!Active) return;
        if (!interrupted) Move(SessionCommand.Stop, "user_request");
        await StopTracksAsync(CaptureEnd.Stop);
        long end = clock.NowNs;
        foreach (RecordingTrack track in tracks)
        {
            if (track.Gap is PendingGap gap)
            {
                try
                {
                    await Task.Run(() => track.Files.AddInterval(gap.Epoch, new CaptureInterval("gap", gap.StartNs, end, gap.Cause)));
                }
                catch (Exception failure) when (failure is IOException or UnauthorizedAccessException or CoreException) { JournalError = true; }
                track.Gap = null;
            }
            if (track.Core.State is not (TrackState.Completed or TrackState.Interrupted))
            {
                if (interrupted)
                {
                    track.Core.Apply(TrackCommand.Interrupt, end);
                }
                else
                {
                    track.Core.Apply(TrackCommand.Stop, end);
                    track.Core.Apply(TrackCommand.Finish, end);
                }
            }
        }
        Move(interrupted ? SessionCommand.Interrupt : SessionCommand.Finish, interrupted ? "source_unavailable" : "capture_drained");
        endedAtNs = clock.NowNs;
        await SaveSessionAsync();
    }

    private void Move(SessionCommand command, string reason)
    {
        long at = clock.NowNs;
        State = core.Apply(command, at);
        transitions.Add(new SessionChange(command.ToString(), State.ToString(), at, reason));
    }

    private async Task SaveSessionAsync(bool required = false)
    {
        var record = new { schema_version = 1, kind = "windows_wasapi_recording", session_id = Id, created_at_utc = created,
            source_clock = "windows_qpc", qpc_origin_100ns = clock.OriginQpc100ns, elapsed_ns = ElapsedNs,
            state = State.ToString(), transitions = transitions.ToArray(),
            tracks = tracks.Select(track => new { id = track.Files.Id, source = track.Files.Endpoint.Source.ToString(),
                source_epoch = track.Core.Epoch, state = track.Core.State.ToString(),
                sealed_segments = track.Files.SealedSegments, saved_through_ns = track.Files.SavedThroughNs }).ToArray() };
        try
        {
            await Task.Run(() => store.Write(new { op = "session_state", id = Id, state = (int)State,
                elapsedNs = record.elapsed_ns, snapshot = record }));
        }
        catch (Exception failure) when (!required && failure is (IOException or UnauthorizedAccessException or CoreException)) { JournalError = true; }
    }

    private async Task ExclusiveAsync(Func<Task> action)
    {
        await operations.WaitAsync();
        try { await action(); }
        finally { operations.Release(); }
    }

    public async ValueTask DisposeAsync()
    {
        if (disposed) return;
        await StopAsync();
        disposed = true;
        foreach (RecordingTrack track in tracks) track.Dispose();
        core.Dispose();
        operations.Dispose();
    }

    private sealed record SessionChange(string Command, string State, long AtNs, string Reason);
}
