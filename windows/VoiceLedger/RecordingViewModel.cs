using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using NAudio.Wave;
using VoiceLedger.Capture;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed record TrackRow(Guid Id, string Source, string Endpoint, string Status, string Saved, bool CanReconnect);

internal sealed class RecordingViewModel : INotifyPropertyChanged, IAsyncDisposable
{
    private static readonly string RecordingRoot = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "VoiceLedger", "Recordings");
    private RecordingSession? session;
    private AudioStore? store;
    private WasapiOut? playback;
    private HistoryTrack? historySelection;
    private string seekTime = "0";
    private string? recoveryStatus;
    private SourceMode mode;
    private CaptureEndpoint? mic;
    private CaptureEndpoint? system;
    private bool busy;
    private bool refreshing;
    private TrackRow[] rows = Array.Empty<TrackRow>();
    private string? error;
    private CancellationTokenSource? liveWorker;
    private bool liveEnabled;
    private string? liveSetup;

    public string[] SpeechLanguages => SpeechWorker.Languages;
    public int SpeechLanguage { get; set; }
    public bool TranslateLive { get; set; }
    public bool LiveEnabled
    {
        get => liveEnabled;
        set
        {
            if (liveEnabled == value) return;
            liveEnabled = value;
            if (value) _ = RefreshLiveModelAsync();
            Update();
        }
    }
    public bool CanConfigureSpeech => CanSelect && LiveEnabled;
    private string? liveStatus;
    public string? LiveStatus => liveStatus ?? (LiveEnabled ? liveSetup : null);
    public string? LiveTranslationStatus { get; private set; }
    public LiveLine[] LiveLines { get; private set; } = Array.Empty<LiveLine>();

    public event PropertyChangedEventHandler? PropertyChanged;
    public SourceMode[] Modes { get; } = Enum.GetValues<SourceMode>();
    public IReadOnlyList<CaptureEndpoint> Microphones { get; private set; } = Array.Empty<CaptureEndpoint>();
    public IReadOnlyList<CaptureEndpoint> RenderEndpoints { get; private set; } = Array.Empty<CaptureEndpoint>();
    public IReadOnlyList<TrackRow> Tracks => rows;
    public IReadOnlyList<HistoryTrack> History { get; private set; } = Array.Empty<HistoryTrack>();

    public HistoryTrack? HistorySelection
    {
        get => historySelection;
        set
        {
            if (historySelection == value) return;
            historySelection = value;
            SeekTime = ((value?.FirstNs ?? 0) / 1_000_000_000m).ToString(CultureInfo.CurrentCulture);
        }
    }

    public string SeekTime
    {
        get => seekTime;
        set { seekTime = value; Update(); }
    }

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
    public bool CanStart => CanSelect && store is not null && playback is null && (mode == SourceMode.System || mic?.IsAvailable == true)
        && (mode == SourceMode.Mic || system?.IsAvailable == true);
    public bool CanPause => !busy && session?.State == SessionState.Recording;
    public bool CanResume => !busy && Active && session!.Tracks.Any(track => track.Core.State == TrackState.Paused);
    public bool CanStop => !busy && Active;
    public bool HasOutput => session is not null || historySelection is not null;
    public bool CanChooseHistory => CanSelect && playback is null;
    public bool CanPlay => CanChooseHistory && historySelection?.CommittedChunks > 0;
    public bool CanStopPlayback => !busy && playback is not null;
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
            if (playback is not null) return "Playing saved audio.";
            if (session is null) return recoveryStatus ?? "Ready";
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

    internal async Task InitializeAsync(bool initial)
    {
        if (store is null)
        {
            await ExecuteAsync(async () =>
            {
                try { store = await Task.Run(() => new AudioStore(RecordingRoot)); }
                catch (CoreException failure) when (failure.Code == AudioStore.Locked)
                {
                    error = "The recording library is open in another VoiceLedger window.";
                    return;
                }
                RecoveryReport report = store.Recovery();
                if (report.InterruptedSessions > 0 || report.RestoredChunks > 0 || report.MissingChunks > 0
                    || report.CorruptChunks > 0 || report.UncommittedChunks > 0 || report.QuarantinedFiles > 0)
                {
                    int gaps = report.MissingChunks + report.CorruptChunks + report.UncommittedChunks;
                    recoveryStatus = $"Recovery · {report.ElapsedMs} ms · {report.InterruptedSessions} interrupted sessions · "
                        + $"{report.RestoredChunks} restored · {gaps} unavailable segments · "
                        + $"known tail loss {report.KnownTailLossNs / 1_000_000d:F1} ms · {report.QuarantinedFiles} quarantined files";
                }
                await RefreshHistoryAsync();
            }, "The recording library could not be recovered. Check available storage.");
            if (store is null) return;
        }
        await RefreshDevicesAsync(initial);
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
        if (Active || playback is not null) return;
        AudioStore library = store ?? throw new InvalidOperationException("Recording storage is unavailable.");
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
        liveWorker?.Cancel();
        liveWorker = null;
        LiveLines = Array.Empty<LiveLine>();
        liveStatus = null;
        LiveTranslationStatus = null;
        RecordingSession recording;
        try { recording = await RecordingSession.StartAsync(library, endpoints); }
        catch (StorageReserveException)
        {
            error = "Free 64 MiB on the recording volume to start.";
            return;
        }
        Volatile.Write(ref session, recording);
        if (recording.Active && LiveEnabled)
            _ = WatchSpeechAsync(recording, SpeechWorker.LanguageCode(SpeechLanguage), TranslateLive);
        else if (!recording.Active)
            await RefreshHistoryAsync();
    }, "Recording could not start. Check the selected endpoints, microphone access and storage.");

    internal Task PauseAsync() => ExecuteAsync(() => session?.PauseAsync() ?? Task.CompletedTask, "Recording could not be paused.");
    internal Task ResumeAsync() => ExecuteAsync(() => session?.ResumeAsync() ?? Task.CompletedTask, "Recording could not resume.");
    internal Task ReconnectAsync(Guid id) => ExecuteAsync(() => session?.ReconnectAsync(id) ?? Task.CompletedTask,
        "The selected endpoint could not reconnect.");
    internal Task StopAsync() => ExecuteAsync(async () =>
    {
        if (session is not null) await session.StopAsync();
        if (!Active) liveWorker?.CancelAfter(TimeSpan.FromSeconds(45));
        await RefreshHistoryAsync();
    }, "Recording could not finish cleanly.");

    internal Task PlayAsync() => ExecuteAsync(async () =>
    {
        if (Active || playback is not null || store is not { } library || historySelection is not { } selected) return;
        if (!decimal.TryParse(SeekTime, NumberStyles.Number, CultureInfo.CurrentCulture, out decimal seconds)
            || seconds < 0 || seconds > long.MaxValue / 1_000_000_000m)
        {
            error = "Enter a valid session time in seconds.";
            return;
        }
        long atNs = (long)(seconds * 1_000_000_000m);
        PlaybackChunk first;
        try { first = await Task.Run(() => library.Seek(selected.Id, atNs)); }
        catch (CoreException failure) when (failure.Code == AudioStore.Unavailable)
        {
            await RefreshHistoryAsync();
            error = "Saved audio is unavailable at that time.";
            return;
        }

        var output = new WasapiOut();
        playback = output;
        try
        {
            output.Init(new CommittedPlayback(at => library.Seek(selected.Id, at), first));
            output.PlaybackStopped += PlaybackStopped;
            output.Play();
        }
        catch
        {
            StopPlayback();
            throw;
        }
    }, "Saved audio could not be played.");

    internal void StopPlayback()
    {
        if (playback is not { } output) return;
        playback = null;
        output.PlaybackStopped -= PlaybackStopped;
        output.Dispose();
        Update();
    }

    private void PlaybackStopped(object? sender, StoppedEventArgs result)
    {
        if (!ReferenceEquals(sender, playback)) return;
        error = result.Exception is null ? "Playback ended." : "Saved audio playback stopped.";
        StopPlayback();
    }

    internal async Task RefreshRecordingAsync()
    {
        if (busy || refreshing || session?.Active != true) return;
        refreshing = true;
        try
        {
            await session.RefreshAsync();
            if (!session.Active)
            {
                liveWorker?.CancelAfter(TimeSpan.FromSeconds(45));
                await RefreshHistoryAsync();
            }
        }
        catch (Exception) { error = "Recording state could not be refreshed."; }
        finally
        {
            refreshing = false;
            Update();
        }
    }

    internal async Task RefreshLiveModelAsync()
    {
        if (!LiveEnabled) return;
        try
        {
            string model = await Task.Run(LiveTranscript.ModelStatus);
            liveSetup = model switch
            {
                "installed" => "Live transcription ready for the next recording.",
                "not_installed" => "Install Whisper Tiny multilingual in Models.",
                _ => "Whisper runtime unavailable.",
            };
        }
        catch (Exception error) when (error is InvalidOperationException or JsonException or TypeInitializationException
            or EntryPointNotFoundException)
        {
            liveSetup = "Could not check the transcription model.";
        }
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LiveStatus)));
    }

    private async Task WatchSpeechAsync(RecordingSession recording, string language, bool translate)
    {
        using var worker = new CancellationTokenSource();
        liveWorker = worker;
        liveStatus = "Starting live transcription…";
        Update();
        string snapshot = SpeechWorker.Request(new { op = "live_transcript", session_id = recording.Id });
        var sources = recording.Tracks.ToDictionary(track => track.Files.Id, track =>
            track.Files.Endpoint.Source == CaptureSource.Mic ? "Microphone" : "System loopback");
        string? jobReason = null;
        async Task RefreshAsync(string partial)
        {
            if (liveWorker != worker) return;
            try
            {
                var (lines, pending, stage, reason) = await Task.Run(() => LiveTranscript.Read(snapshot, partial, sources));
                if (liveWorker != worker) return;
                jobReason = reason;
                string? message = stage switch
                {
                    "loading" => "Loading Whisper Tiny multilingual…",
                    "waiting_for_audio" => "Waiting for the first saved audio segment…",
                    "listening" when lines.Length == 0 => "Listening for speech…",
                    "listening" => "Live transcription active.",
                    _ => liveStatus,
                };
                if (message != liveStatus)
                {
                    liveStatus = message;
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LiveStatus)));
                }
                if (!LiveLines.SequenceEqual(lines))
                {
                    LiveLines = lines;
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LiveLines)));
                }
                string? status = translate && pending ? "Simplified Chinese translations pending" : null;
                if (LiveTranslationStatus != status)
                {
                    LiveTranslationStatus = status;
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LiveTranslationStatus)));
                }
            }
            catch (Exception error) when (error is InvalidOperationException or JsonException or TypeInitializationException
                or EntryPointNotFoundException) { }
        }

        int result = -31;
        try
        {
            bool profiled = await Task.Run(() => DeviceProfiler.Capture(SpeechWorker.ProfileDirectory));
            if (liveWorker != worker) return;
            result = profiled
                ? await SpeechWorker.RunAsync(SpeechWorker.Request(new
                    { op = "transcribe_live", session_id = recording.Id, language, translate, approved = true }),
                    RefreshAsync, worker.Token)
                : -34;
            await RefreshAsync("");
        }
        finally
        {
            if (liveWorker == worker)
            {
                liveWorker = null;
                liveStatus = result switch
                {
                    0 when LiveLines.Length == 0 => "Recording ended without recognized speech.",
                    0 => "Transcription complete.",
                    -30 => "Could not read or save the local transcript.",
                    -32 => "No committed audio for transcription.",
                    -33 => "Install Whisper Tiny multilingual in Models.",
                    -36 => "Whisper model checksum failed.",
                    -37 => "Another local speech task is running.",
                    -38 => jobReason switch
                    {
                        "memory_pressure" => "Transcription paused due to low memory.",
                        "thermal_pressure" => "Transcription paused due to high temperature.",
                        "battery_pressure" => "Transcription paused due to low battery.",
                        "storage_pressure" => "Transcription paused due to low storage.",
                        _ => "Transcription deferred. Resume in Sessions.",
                    },
                    -34 => "Device cannot load Whisper safely. Check Models.",
                    -4 => "Transcription paused. Resume in Sessions.",
                    _ => "Local speech worker unavailable. Retry in Sessions.",
                };
                Update();
            }
        }
    }

    internal void Suspend() => Volatile.Read(ref session)?.Suspend();

    internal void OpenFolder()
    {
        string? path = session?.OutputDirectory;
        if (!Active && historySelection is { } selected && store is { } library)
            path = library.SessionDirectory(selected.SessionId);
        if (path is null) return;
        try { Process.Start(new ProcessStartInfo(path) { UseShellExecute = true }); }
        catch (Exception)
        {
            error = "The recording folder could not be opened.";
            Update();
        }
    }

    private async Task RefreshHistoryAsync()
    {
        if (store is not { } library) return;
        Guid? selectedId = historySelection?.Id;
        HistoryTrack[] rows = await Task.Run(library.History);
        History = rows;
        HistorySelection = rows.FirstOrDefault(track => track.Id == selectedId) ?? rows.FirstOrDefault();
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
                ? $"Gap · {FailureLabel(reason)}"
                : track.Run?.QueuedPackets >= CaptureRun.QueuePackets * 3 / 4 ? "Writer lag" : track.Core.State.ToString(),
            $"{track.Files.SealedSegments} saved segments · epoch {track.Core.Epoch} · queue {track.Run?.QueuedPackets ?? 0}/{CaptureRun.QueuePackets}",
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
        CaptureEnd.StorageExhausted => "Storage is below the 64 MiB reserve",
        CaptureEnd.UnsupportedFormat => "Endpoint PCM format unsupported",
        _ => "Audio source failed"
    };

    public async ValueTask DisposeAsync()
    {
        liveWorker?.Cancel();
        liveWorker = null;
        StopPlayback();
        if (session is not null) await session.DisposeAsync();
        store?.Dispose();
    }
}
