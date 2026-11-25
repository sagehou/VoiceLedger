using System;
using System.Globalization;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using NAudio.Wave;
using VoiceLedger.Capture;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed class SessionAudioPanel : StackPanel, IDisposable
{
    private const double Second = 1_000_000_000.0;

    private readonly string session;
    private readonly Func<bool> canPlay;
    private readonly ComboBox tracks = new();
    private readonly ListBox timeline = new() { MaxHeight = 180 };
    private readonly TextBox position = new() { MaxLength = 24 };
    private readonly Button play = new() { Content = "Play track", HorizontalAlignment = HorizontalAlignment.Left };
    private readonly Button stop = new() { Content = "Stop playback", HorizontalAlignment = HorizontalAlignment.Left };
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap };
    private WasapiOut? player;
    private int selectionVersion;
    private int playVersion;
    private bool disposed;

    internal event Action<string?>? TrackChanged;

    internal SessionAudioPanel(string session, JsonElement savedTracks, Func<bool> canPlay)
    {
        this.session = session;
        this.canPlay = canPlay;
        tracks.Items.Add(new ComboBoxItem { Content = "All tracks" });
        foreach (JsonElement saved in savedTracks.EnumerateArray())
        {
            string source = Read(saved, "source");
            var track = new Track(Read(saved, "id"), source, saved.GetProperty("first_ns").GetInt64(),
                saved.GetProperty("committed_chunks").GetInt64() > 0);
            tracks.Items.Add(new ComboBoxItem { Content = source == "mic" ? "Microphone" : "System loopback", Tag = track });
        }
        AutomationProperties.SetName(tracks, "Source track filter");
        AutomationProperties.SetName(position, "Start time in seconds");
        AutomationProperties.SetName(timeline, "Track timeline");
        Children.Add(tracks);
        Children.Add(timeline);
        Children.Add(position);
        Children.Add(play);
        Children.Add(stop);
        Children.Add(status);
        tracks.SelectionChanged += async (_, _) => await LoadTrackAsync();
        play.Click += async (_, _) => await PlayAsync();
        stop.Click += (_, _) => Stop();
        tracks.SelectedIndex = 0;
    }

    private Track? Selected => (tracks.SelectedItem as ComboBoxItem)?.Tag as Track;

    private async Task LoadTrackAsync()
    {
        int version = ++selectionVersion;
        Stop();
        Track? selected = Selected;
        TrackChanged?.Invoke(selected?.Source);
        timeline.ItemsSource = null;
        timeline.Visibility = selected is null ? Visibility.Collapsed : Visibility.Visible;
        position.Visibility = play.Visibility = stop.Visibility =
            selected?.HasAudio == true ? Visibility.Visible : Visibility.Collapsed;
        status.Text = selected is { HasAudio: false } ? "No audio saved." : "";
        if (selected is null) return;
        position.Text = (selected.FirstNs / Second).ToString("G", CultureInfo.CurrentCulture);
        try
        {
            var rows = await Task.Run(() =>
            {
                using var doc = JsonDocument.Parse(Request(new { op = "track_timeline", session_id = session, track_id = selected.Id }));
                return doc.RootElement.EnumerateArray().Select(row =>
                {
                    string kind = Read(row, "kind") switch
                    {
                        "silence" => "Silence",
                        "gap" => "Gap",
                        "pending" => "Pending audio",
                        _ => "Saved audio",
                    };
                    double start = row.GetProperty("start_ns").GetInt64() / Second;
                    double end = row.GetProperty("end_ns").GetInt64() / Second;
                    return $"{kind} · {start:F2}–{end:F2} s";
                }).ToArray();
            });
            if (!disposed && version == selectionVersion) timeline.ItemsSource = rows;
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException or TypeInitializationException)
        {
            if (!disposed && version == selectionVersion) status.Text = "The track timeline is unavailable.";
        }
    }

    private async Task PlayAsync()
    {
        if (disposed || player is not null || !canPlay() || Selected is not { } track) return;
        if (!decimal.TryParse(position.Text, NumberStyles.Number, CultureInfo.CurrentCulture, out decimal seconds)
            || seconds < 0 || seconds > long.MaxValue / 1_000_000_000m)
        {
            status.Text = "Enter a valid start time in seconds.";
            return;
        }
        int version = ++playVersion;
        long start = (long)(seconds * 1_000_000_000m);
        PlaybackChunk Seek(long time) => AudioStore.DecodePlayback(Request(new
        {
            op = "playback", session_id = session, track_id = track.Id, position_ns = time,
        }), time);
        play.IsEnabled = false;
        try
        {
            PlaybackChunk first = await Task.Run(() => Seek(start));
            if (disposed || version != playVersion || !canPlay()) return;
            var output = new WasapiOut();
            player = output;
            output.PlaybackStopped += (_, result) => Dispatcher.InvokeAsync(() =>
            {
                if (!ReferenceEquals(player, output)) return;
                Stop(result.Exception is null ? "Playback ended." : "Saved audio playback stopped.");
            });
            output.Init(new CommittedPlayback(Seek, first));
            output.Play();
            status.Text = "Playing saved audio";
        }
        catch (Exception failure) when (failure is InvalidOperationException or ArgumentException
            or COMException or System.IO.IOException or TypeInitializationException)
        {
            if (!disposed && version == playVersion)
                Stop(failure is CoreException { Code: NativeCore.SpeechNoAudio } ? "No saved audio at that time." : "Could not play the saved track.");
        }
        finally
        {
            if (version == playVersion) play.IsEnabled = player is null;
        }
    }

    private void Stop(string message = "")
    {
        playVersion++;
        var output = player;
        player = null;
        output?.Dispose();
        play.IsEnabled = true;
        status.Text = message;
    }

    public void Dispose()
    {
        disposed = true;
        Stop();
    }

    private static byte[] Request(object action) => NativeCore.SpeechRequest(SpeechWorker.Request(action));
    private static string Read(JsonElement element, string property) => element.GetProperty(property).GetString() ?? "";
    private sealed record Track(string Id, string Source, long FirstNs, bool HasAudio);
}
