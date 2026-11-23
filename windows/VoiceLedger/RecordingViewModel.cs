using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using VoiceLedger.Capture;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed record TrackRow(Guid Id, string Source, string Endpoint, string Status, string Saved, bool CanReconnect);

internal sealed class RecordingViewModel : INotifyPropertyChanged, IAsyncDisposable
{
    private static readonly string RecordingRoot = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "VoiceLedger", "Recordings");
    private RecordingSession? session;
    private SourceMode mode;
    private CaptureEndpoint? mic;
    private CaptureEndpoint? system;
    private bool busy;
    private bool refreshing;
    private TrackRow[] rows = Array.Empty<TrackRow>();
    private string? error;

    public event PropertyChangedEventHandler? PropertyChanged;
    public SourceMode[] Modes { get; } = Enum.GetValues<SourceMode>();
    public IReadOnlyList<CaptureEndpoint> Microphones { get; private set; } = Array.Empty<CaptureEndpoint>();
    public IReadOnlyList<CaptureEndpoint> RenderEndpoints { get; private set; } = Array.Empty<CaptureEndpoint>();
    public IReadOnlyList<TrackRow> Tracks => rows;

    public SourceMode Mode
    {
        get => mode;
        set { mode = value; Update(); }
    }

    public CaptureEndpoint? Microphone
    {
        get => mic;
        set { mic = value; Update(); }
    }

    public CaptureEndpoint? SystemEndpoint
    {
        get => system;
        set { system = value; Update(); }
    }

    public bool Active => session?.Active == true;
    public bool Busy => busy;
    public bool CanSelect => !busy && !Active;
    public bool CanSelectMic => CanSelect && mode != SourceMode.System;
    public bool CanSelectSystem => CanSelect && mode != SourceMode.Mic;
    public bool CanStart => CanSelect && (mode == SourceMode.System || mic?.IsAvailable == true)
        && (mode == SourceMode.Mic || system?.IsAvailable == true);
    public bool CanPause => !busy && session?.State == SessionState.Recording;
    public bool CanResume => !busy && Active && session!.Tracks.Any(track => track.Core.State == TrackState.Paused);
    public bool CanStop => !busy && Active;
    public bool HasOutput => session is not null;
    public string Duration
    {
        get
        {
            var elapsed = TimeSpan.FromTicks((session?.ElapsedNs ?? 0) / 100);
            return $"{(long)elapsed.TotalHours:00}:{elapsed.Minutes:00}:{elapsed.Seconds:00}";
        }
    }

    public string Status
    {
        get
        {
            if (error is not null) return error;
            if (busy) return "Updating recording…";
            if (session is null) return "Ready";
            if (session.JournalError) return "Recording metadata could not be saved. Check available storage.";
            if (session.State == SessionState.Paused && session.Tracks.All(track => track.Core.State != TrackState.Paused))
                return "Recording paused. Reconnect a selected endpoint or stop.";
            if (session.State is SessionState.Completed or SessionState.Interrupted)
            {
                int saved = session.Tracks.Sum(track => track.Files.SealedSegments);
                bool hasLoss = session.Tracks.Any(track => track.HadLoss || track.Files.LossIntervals > 0);
                string result = saved == 0 ? "No audio segments were saved." : $"{saved} audio segments saved.";
                return hasLoss ? $"Capture ended with gaps. {result}" : $"Capture ended. {result}";
            }
            return session.State.ToString();
        }
    }

    internal Task RefreshDevicesAsync(bool initial = false) => ExecuteAsync(async () =>
    {
        if (Active) return;
        string? previousMic = mic?.Id;
        string? previousSystem = system?.Id;
        var (endpoints, diagnostic) = await Task.Run(EndpointCatalog.Read);
        Microphones = endpoints.Where(item => item.Source == CaptureSource.Mic).ToArray();
        RenderEndpoints = endpoints.Where(item => item.Source == CaptureSource.System).ToArray();
        mic = Selection(Microphones, previousMic, initial);
        system = Selection(RenderEndpoints, previousSystem, initial);
        error = diagnostic ?? (endpoints.Any(item => item.IsAvailable) ? null : "No active audio endpoints found.");
    }, "Audio endpoints could not be listed. Refresh devices to try again.");

    internal Task StartAsync() => ExecuteAsync(async () =>
    {
        if (Active) return;
        var endpoints = new List<CaptureEndpoint>();
        if (mode != SourceMode.System && mic is { } microphone) endpoints.Add(microphone);
        if (mode != SourceMode.Mic && system is { } renderEndpoint) endpoints.Add(renderEndpoint);
        int requested = mode == SourceMode.Both ? 2 : 1;
        if (endpoints.Count != requested) throw new InvalidOperationException("Select the requested endpoints.");
        if (session is not null)
        {
            await session.DisposeAsync();
            Volatile.Write(ref session, null);
        }
        RecordingSession recording = await RecordingSession.StartAsync(RecordingRoot, endpoints);
        Volatile.Write(ref session, recording);
    }, "Recording could not start. Check the selected endpoints, microphone access and storage.");

    internal Task PauseAsync() => ExecuteAsync(() => session?.PauseAsync() ?? Task.CompletedTask, "Recording could not be paused.");
    internal Task ResumeAsync() => ExecuteAsync(() => session?.ResumeAsync() ?? Task.CompletedTask, "Recording could not resume.");
    internal Task ReconnectAsync(Guid id) => ExecuteAsync(() => session?.ReconnectAsync(id) ?? Task.CompletedTask,
        "The selected endpoint could not reconnect.");
    internal Task StopAsync() => ExecuteAsync(() => session?.StopAsync() ?? Task.CompletedTask, "Recording could not finish cleanly.");

    internal async Task RefreshRecordingAsync()
    {
        if (busy || refreshing || session?.Active != true) return;
        refreshing = true;
        try { await session.RefreshAsync(); }
        catch (Exception) { error = "Recording state could not be refreshed."; }
        finally
        {
            refreshing = false;
            Update();
        }
    }

    internal void Suspend() => Volatile.Read(ref session)?.Suspend();

    internal void OpenFolder()
    {
        if (session is null) return;
        try { Process.Start(new ProcessStartInfo(session.OutputDirectory) { UseShellExecute = true }); }
        catch (Exception)
        {
            error = "The recording folder could not be opened.";
            Update();
        }
    }

    private async Task ExecuteAsync(Func<Task> operation, string failure)
    {
        if (busy) return;
        busy = true;
        error = null;
        Update();
        try { await operation(); }
        catch (Exception) { error = failure; }
        finally
        {
            busy = false;
            Update();
        }
    }

    private void Update()
    {
        TrackRow[] updated = session?.Tracks.Select(track => new TrackRow(track.Files.Id,
            track.Files.Endpoint.Source == CaptureSource.Mic ? "Microphone" : "System loopback",
            track.Files.Endpoint.Name,
            track.LastEnd is CaptureEnd reason && reason is not (CaptureEnd.Stop or CaptureEnd.Pause)
                ? $"Gap · {FailureLabel(reason)}" : track.Core.State.ToString(),
            $"{track.Files.SealedSegments} saved segments · epoch {track.Core.Epoch}",
            !busy && Active && track.Core.State == TrackState.Disconnected)).ToArray() ?? Array.Empty<TrackRow>();
        if (!rows.SequenceEqual(updated)) rows = updated;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(null));
    }

    private static CaptureEndpoint? Selection(IReadOnlyList<CaptureEndpoint> endpoints, string? previous, bool initial) =>
        initial ? endpoints.FirstOrDefault(item => item.IsDefault && item.IsAvailable)
            : endpoints.FirstOrDefault(item => StringComparer.Ordinal.Equals(previous, item.Id));

    private static string FailureLabel(CaptureEnd reason) => reason switch
    {
        CaptureEnd.EndpointLost => "Endpoint disconnected",
        CaptureEnd.FormatChanged => "Endpoint format changed",
        CaptureEnd.SystemSuspend => "System suspended",
        CaptureEnd.QueueOverflow => "Writer queue full",
        CaptureEnd.StorageFailure => "Storage write failed",
        _ => "Audio source failed"
    };

    public async ValueTask DisposeAsync()
    {
        if (session is not null) await session.DisposeAsync();
    }
}
