using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using Microsoft.Win32;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed class ModelsWindow : Window
{
    private readonly StackPanel content = new() { Margin = new Thickness(20) };
    private readonly TextBlock partial = new() { TextWrapping = TextWrapping.Wrap };
    private readonly Func<bool> canWork;
    private readonly ComboBox language = new()
    {
        ItemsSource = SpeechWorker.Languages, SelectedIndex = 0,
    };
    private readonly bool sessions;
    private readonly TextBox search = new() { MaxLength = 64 };
    private string? selectedSession;
    private SessionAudioPanel? sessionAudio;
    private string? transcriptTrack;
    private JsonElement? transcriptAfter;
    private readonly Stack<JsonElement?> priorPages = new();
    private int transcriptPageVersion;
    private int loadVersion;
    private bool showModels;
    private bool showAudio;
    private int searchVersion;
    private CancellationTokenSource? worker;
    private bool cancelled;
    private bool closed;
    private bool working;

    internal ModelsWindow(Func<bool> canWork, bool sessions = false, string? initialSession = null)
    {
        this.canWork = canWork;
        this.sessions = sessions;
        selectedSession = initialSession;
        Title = sessions ? "Sessions" : "Models";
        Width = sessions ? 700 : 520;
        Height = sessions ? 660 : 400;
        MinWidth = 320;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Content = new ScrollViewer { Content = content, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        AddLine(sessions ? "Loading sessions…" : "Loading models…");
        Loaded += async (_, _) => await LoadAsync();
    }

    protected override void OnClosed(EventArgs e)
    {
        closed = true;
        sessionAudio?.Dispose();
        Cancel();
        base.OnClosed(e);
    }

    private async Task BrowseSessionsAsync()
    {
        content.Children.Clear();
        AutomationProperties.SetName(search, "Search saved sessions");
        var submit = new Button { Content = "Search sessions", HorizontalAlignment = HorizontalAlignment.Left };
        submit.Click += async (_, _) => await BrowseSessionsAsync();
        content.Children.Add(search);
        content.Children.Add(submit);
        string query = search.Text;
        int version = ++searchVersion;
        try
        {
            byte[] data = await Task.Run(() => NativeCore.SpeechRequest(
                SpeechWorker.Request(new { op = "history", query })));
            if (closed || version != searchVersion || selectedSession is not null) return;
            using var history = JsonDocument.Parse(data);
            foreach (JsonElement session in history.RootElement.EnumerateArray())
            {
                string id = Read(session, "id");
                var date = DateTimeOffset.FromUnixTimeMilliseconds(session.GetProperty("created_at_ms").GetInt64());
                string title = Read(session, "title");
                string name = string.IsNullOrEmpty(title) ? "" : $"{title}\n";
                var duration = TimeSpan.FromSeconds(session.GetProperty("elapsed_ns").GetInt64() / 1_000_000_000.0);
                string state = session.GetProperty("interrupted").GetBoolean() ? "Interrupted" : "Completed";
                long saved = session.GetProperty("committed").GetInt64();
                long missing = session.GetProperty("missing").GetInt64();
                string transcript = session.GetProperty("has_transcript").GetBoolean() ? "Transcript saved" : "No transcript";
                string availability = session.GetProperty("text_only").GetBoolean()
                    ? "Synced text · Audio unavailable on this device" : $"Audio {saved} saved · {missing} missing · {transcript}";
                var item = new Button
                {
                    Content = new TextBlock
                    {
                        Text = $"{name}{date.LocalDateTime:g} · {duration:g} · {state}\n{availability}",
                        TextWrapping = TextWrapping.Wrap,
                    },
                    Margin = new Thickness(0, 6, 0, 0),
                };
                item.Click += async (_, _) =>
                {
                    if (working || selectedSession is not null) return;
                    selectedSession = id;
                    ResetNote();
                    await LoadAsync();
                };
                content.Children.Add(item);
            }
            if (history.RootElement.GetArrayLength() == 0)
            {
                AddLine(query.Length == 0 ? "No saved sessions." : "No matching sessions.");
                var back = new Button { Content = "Return to recording", HorizontalAlignment = HorizontalAlignment.Left };
                back.Click += (_, _) => Close();
                content.Children.Add(back);
            }
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException or TypeInitializationException)
        {
            if (!closed && version == searchVersion) AddLine("Saved sessions are unavailable.");
        }
    }

    private async Task LoadAsync(string? status = null)
    {
        sessionAudio?.Dispose();
        sessionAudio = null;
        ++transcriptPageVersion;
        int version = ++loadVersion;
        if (sessions && selectedSession is null)
        {
            await BrowseSessionsAsync();
            return;
        }
        try
        {
            string? requestedSession = selectedSession;
            var (catalog, inspection) = await Task.Run(() =>
            {
                string directory = SpeechWorker.ProfileDirectory;
                DeviceProfiler.Capture(directory);
                return (NativeCore.ModelPreview(directory), NativeCore.SpeechRequest(SpeechWorker.Request(new { op = "inspect", session_id = requestedSession })));
            });
            if (closed || version != loadVersion) return;
            using var report = JsonDocument.Parse(catalog);
            using var speech = JsonDocument.Parse(inspection);
            JsonElement session = speech.RootElement.GetProperty("session");
            string? sessionId = session.ValueKind == JsonValueKind.Null ? null : Read(session, "id");
            JsonElement lines = speech.RootElement.GetProperty("lines");
            bool hasEnglish = speech.RootElement.GetProperty("has_translation_source").GetBoolean();
            bool textOnly = speech.RootElement.GetProperty("text_only").GetBoolean();
            content.Children.Clear();
            if (sessions)
            {
                var navigation = new WrapPanel();
                var back = new Button { Content = "Back to sessions", Margin = new Thickness(0, 0, 8, 0) };
                back.Click += async (_, _) =>
                {
                    if (working) return;
                    selectedSession = null;
                    ResetNote();
                    await LoadAsync();
                };
                navigation.Children.Add(back);
                if (sessionId is not null)
                {
                    var refresh = new Button { Content = "Refresh note" };
                    refresh.Click += async (_, _) => { if (!working) await LoadAsync(); };
                    navigation.Children.Add(refresh);
                }
                content.Children.Add(navigation);
            }
            if (status is not null) AddLine(status);
            else if (speech.RootElement.GetProperty("job") is { ValueKind: JsonValueKind.Object } job
                && JobStatus(job) is { } text) AddLine(text);
            var modelControls = new StackPanel();
            var transcript = new StackPanel();
            var models = new Expander
            {
                Header = "Transcription and translation models", Content = modelControls, IsExpanded = showModels,
                Margin = new Thickness(0, 8, 0, 8),
            };
            models.Expanded += (_, _) => showModels = true;
            models.Collapsed += (_, _) => showModels = false;
            if (sessionId is not null)
            {
                string title = Read(speech.RootElement, "title");
                if (sessions) AddHeading(title.Length > 0 ? title : "Session note", 25);
                var created = DateTimeOffset.FromUnixTimeMilliseconds(session.GetProperty("created_at_ms").GetInt64());
                AddLine($"{created.LocalDateTime:g}");
                if (textOnly) AddLine("Synced text · Audio unavailable on this device");
                else
                {
                    AddLine("Transcription language", panel: modelControls);
                    if (language.Parent is Panel previousPanel) previousPanel.Children.Remove(language);
                    modelControls.Children.Add(language);
                }
                if (sessions)
                {
                    ShowTextEditor("Rename session", title, "Title saved.", text => new
                    {
                        op = "rename_session", session_id = sessionId, text,
                    }, maxLength: 64, allowBlank: true);
                    bool hasText = lines.GetArrayLength() > 0;
                    if (hasText || speech.RootElement.GetProperty("summary").GetProperty("current").ValueKind == JsonValueKind.Object)
                        AddExport(sessionId);
                    AddSummary(sessionId, speech.RootElement.GetProperty("summary"),
                        speech.RootElement.GetProperty("summary_providers").Clone(), hasText);
                    if (!textOnly)
                    {
                        JsonElement tracks = speech.RootElement.GetProperty("tracks").Clone();
                        sessionAudio = new SessionAudioPanel(sessionId, tracks, canWork, transcriptTrack);
                        sessionAudio.TrackChanged += async track =>
                        {
                            transcriptTrack = track;
                            transcriptAfter = null;
                            priorPages.Clear();
                            await LoadTranscriptPageAsync(sessionId, transcript, models);
                        };
                        var audio = new Expander { Header = "Audio and playback", Content = sessionAudio, IsExpanded = showAudio };
                        audio.Expanded += (_, _) => showAudio = true;
                        audio.Collapsed += (_, _) => showAudio = false;
                        content.Children.Add(audio);
                        if (tracks.EnumerateArray().Any(track => track.GetProperty("committed_chunks").GetInt64() > 0))
                        {
                            JsonElement providers = speech.RootElement.GetProperty("external_transcription_providers").Clone();
                            var external = new Button
                            {
                                Content = "Transcribe with external service…", HorizontalAlignment = HorizontalAlignment.Left,
                                IsEnabled = canWork(),
                            };
                            external.Click += async (_, _) =>
                            {
                                if (closed || working || !canWork()) return;
                                var dialog = new ExternalModelWindow(this, sessionId, providers, tracks, language.SelectedIndex);
                                if (dialog.ShowDialog() == true && dialog.Request is { } action)
                                    await RunAsync(action, "transcribe_external");
                            };
                            modelControls.Children.Add(external);
                        }
                    }
                }
            }
            else if (sessions)
            {
                AddLine("This saved note is unavailable.");
                return;
            }
            if (sessions && !textOnly) content.Children.Add(models);
            if (!sessions) content.Children.Add(modelControls);
            foreach (JsonElement candidate in report.RootElement.GetProperty("candidates").EnumerateArray())
            {
                JsonElement model = candidate.GetProperty("model");
                JsonElement runtime = model.GetProperty("runtime");
                string id = Read(model, "id");
                JsonElement choice = speech.RootElement.GetProperty("models").EnumerateArray()
                    .First(option => Read(option, "id") == id);
                bool installed = choice.GetProperty("installed").GetBoolean();
                bool translation = model.TryGetProperty("translation", out _);
                string? blockedBy = choice.GetProperty("blocked_by").GetString();
                AddLine($"{Read(model, "name")} · {Read(model, "quantization")}", bold: true, panel: modelControls);
                AddLine($"{Read(runtime, "id")} {Read(runtime, "version")} · {Read(model.GetProperty("license"), "id")}", panel: modelControls);
                string languages = string.Join(", ", model.GetProperty("languages").EnumerateArray().Select(language => language.GetString()));
                AddLine($"Languages {languages} · Download {Mib(candidate.GetProperty("download_bytes").GetInt64())}", panel: modelControls);
                JsonElement ram = model.GetProperty("peak_ram_estimate");
                AddLine(ram.ValueKind == JsonValueKind.Null
                    ? "Peak RAM estimate unavailable"
                    : $"Estimated peak RAM {Mib(ram.GetProperty("bytes").GetInt64())}", panel: modelControls);
                AddLine($"Benchmark pending · {Status(blockedBy)}", panel: modelControls);
                var action = new Button
                {
                    Content = !installed ? "Download and verify"
                        : translation ? "Translate English transcript" : "Transcribe saved session",
                    HorizontalAlignment = HorizontalAlignment.Left,
                    Padding = new Thickness(10, 4, 10, 4),
                    IsEnabled = blockedBy is null && canWork()
                        && (!installed || (sessionId is not null && !textOnly && (!translation || hasEnglish))),
                };
                action.Click += async (_, _) =>
                {
                    var policy = SpeechWorker.LanguageCode(language.SelectedIndex);
                    string operation = !installed ? "install" : translation ? "translate" : "transcribe";
                    object request = operation switch
                    {
                        "install" => new { op = operation, model_id = id, approved = true },
                        "translate" => new { op = operation, model_id = id, session_id = sessionId, approved = true },
                        _ => new { op = operation, model_id = id, session_id = sessionId, language = policy, approved = true },
                    };
                    await RunAsync(request, operation);
                };
                modelControls.Children.Add(action);
                if (translation && installed && !hasEnglish)
                    action.ToolTip = "Transcribe an English recording first.";
            }
            if (sessions && sessionId is not null)
            {
                AddHeading("Transcript");
                content.Children.Add(transcript);
                await LoadTranscriptPageAsync(sessionId, transcript, textOnly ? null : models);
            }
            else if (sessionId is not null)
            {
                foreach (JsonElement line in lines.EnumerateArray()) AddTranscriptLine(content, sessionId, line);
            }
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            if (closed || version != loadVersion) return;
            if (sessions) AddLine("Could not refresh the saved note.");
            else
            {
                content.Children.Clear();
                AddLine("Model catalog unavailable. Recording only.");
            }
        }
    }

    private void ResetNote()
    {
        transcriptTrack = null;
        transcriptAfter = null;
        priorPages.Clear();
        showModels = false;
        showAudio = false;
    }

    private async Task LoadTranscriptPageAsync(string sessionId, StackPanel transcript, Expander? models)
    {
        int version = ++transcriptPageVersion;
        transcript.Children.Clear();
        AddLine("Loading saved transcript…", panel: transcript);
        try
        {
            string request = SpeechWorker.Request(new
            {
                op = "transcript_page", session_id = sessionId, after = transcriptAfter, track_id = transcriptTrack,
            });
            byte[] bytes = await Task.Run(() => NativeCore.SpeechRequest(request));
            if (closed || version != transcriptPageVersion) return;
            using var report = JsonDocument.Parse(bytes);
            transcript.Children.Clear();
            JsonElement lines = report.RootElement.GetProperty("lines");
            if (lines.GetArrayLength() == 0)
            {
                AddLine(transcriptTrack is null ? "No saved transcript is available." : "No saved transcript on this track.",
                    panel: transcript);
                if (models is not null)
                {
                    var transcribe = new Button { Content = "Choose transcription model", HorizontalAlignment = HorizontalAlignment.Left };
                    transcribe.Click += (_, _) => { models.IsExpanded = true; models.BringIntoView(); };
                    transcript.Children.Add(transcribe);
                }
            }
            foreach (JsonElement line in lines.EnumerateArray()) AddTranscriptLine(transcript, sessionId, line);
            JsonElement next = report.RootElement.GetProperty("next");
            var pages = new WrapPanel { Margin = new Thickness(0, 12, 0, 0) };
            if (priorPages.Count > 0)
            {
                var previous = new Button { Content = "Previous page", Margin = new Thickness(0, 0, 8, 0) };
                previous.Click += async (_, _) =>
                {
                    transcriptAfter = priorPages.Pop();
                    await LoadTranscriptPageAsync(sessionId, transcript, models);
                    transcript.BringIntoView();
                };
                pages.Children.Add(previous);
            }
            if (next.ValueKind != JsonValueKind.Null)
            {
                var cursor = next.Clone();
                var more = new Button { Content = "Next page" };
                more.Click += async (_, _) =>
                {
                    priorPages.Push(transcriptAfter);
                    transcriptAfter = cursor;
                    await LoadTranscriptPageAsync(sessionId, transcript, models);
                    transcript.BringIntoView();
                };
                pages.Children.Add(more);
            }
            if (pages.Children.Count > 0) transcript.Children.Add(pages);
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            if (closed || version != transcriptPageVersion) return;
            transcript.Children.Clear();
            AddLine("Could not load the saved transcript page.", panel: transcript);
            var retry = new Button { Content = "Reload transcript", HorizontalAlignment = HorizontalAlignment.Left };
            retry.Click += async (_, _) => await LoadTranscriptPageAsync(sessionId, transcript, models);
            transcript.Children.Add(retry);
        }
    }

    private void AddTranscriptLine(StackPanel panel, string sessionId, JsonElement line)
    {
        var row = new StackPanel { Margin = new Thickness(0, 0, 0, 14) };
        string source = Read(line, "source") == "mic" ? "Microphone" : "System loopback";
        string detected = SpeechWorker.LanguageName(Read(line, "language"));
        var time = TimeSpan.FromTicks(line.GetProperty("start_ns").GetInt64() / 100);
        JsonElement speaker = line.GetProperty("speaker");
        bool labeled = speaker.ValueKind == JsonValueKind.Object;
        string name = labeled ? Read(speaker, "name") : "";
        string metadata = name.Length > 0 ? $" · {name}" : "";
        AddLine($"{(long)time.TotalHours:00}:{time.Minutes:00}:{time.Seconds:00} · {source} · {detected}{metadata}", bold: true, panel: row);
        AddLine(Read(line, "text"), panel: row, body: sessions);

        string utterance = Read(line, "utterance_id");
        string revision = Read(line, "revision_id");
        JsonElement conflicts = line.GetProperty("conflicts");
        bool needsReview = conflicts.GetArrayLength() > 1;
        var edits = new StackPanel();
        if (needsReview)
        {
            foreach (JsonElement item in conflicts.EnumerateArray())
            {
                string author = item.GetProperty("manual").GetBoolean() ? "Manual" : "Machine";
                AddLine($"{author} · {SpeechWorker.LanguageName(Read(item, "language"))}", bold: true, panel: edits);
                AddLine(Read(item, "text"), panel: edits);
            }
            string[] heads = conflicts.EnumerateArray().Select(item => Read(item, "revision_id")).ToArray();
            ShowTextEditor("Resolve conflict", Read(line, "text"), "Merged revision saved.", text => new
            {
                op = "resolve_conflict", session_id = sessionId, utterance_id = utterance, expected_heads = heads, text,
            }, panel: edits);
        }
        else
        {
            ShowTextEditor("Edit transcript", Read(line, "text"), "Transcript saved.", text => new
            {
                op = "save_utterance", session_id = sessionId, utterance_id = utterance, expected_revision_id = revision, text,
            }, panel: edits);
            if (sessions && line.TryGetProperty("external_draft", out JsonElement externalDraft)
                && externalDraft.GetString() is { } draft)
            {
                ShowTextEditor("Review external draft", draft, "Transcript saved.", text => new
                {
                    op = "save_utterance", session_id = sessionId, utterance_id = utterance, expected_revision_id = revision, text,
                }, panel: edits);
            }
        }
        ShowTranslation(sessionId, line, row, edits);
        if (sessions)
        {
            ShowTextEditor("Edit speaker name", name, "Speaker saved.", text => new
            {
                op = "save_speaker", session_id = sessionId, utterance_id = utterance,
                expected_revision_id = revision, expected_speaker_id = labeled ? Read(speaker, "revision_id") : null, text,
            }, maxLength: 64, allowBlank: true, panel: edits);
            row.Children.Add(new Expander
            {
                Header = needsReview ? "Review conflicting revisions" : "Edit and review",
                Content = edits,
            });
        }
        else row.Children.Add(edits);
        panel.Children.Add(row);
    }

    private void AddSummary(string sessionId, JsonElement summary, JsonElement providers, bool hasText)
    {
        AddHeading("Summary");
        JsonElement current = summary.GetProperty("current");
        JsonElement draft = summary.GetProperty("draft");
        bool exists = current.ValueKind == JsonValueKind.Object;
        string? expected = exists ? Read(current, "id") : null;

        var generate = new Button
        {
            Content = "Generate with external model…",
            HorizontalAlignment = HorizontalAlignment.Left,
            IsEnabled = hasText && canWork(),
        };
        generate.Click += async (_, _) =>
        {
            if (closed || working || !canWork()) return;
            var dialog = new ExternalModelWindow(this, sessionId, providers);
            if (dialog.ShowDialog() == true && dialog.Request is { } request)
                await RunAsync(request, "summarize");
        };

        if (exists)
        {
            if (current.GetProperty("stale").GetBoolean()) AddLine("Transcript changed since this summary.");
            AddLine(Read(current, "text"), body: true);
        }
        else AddLine("No summary yet.");

        ShowTextEditor(exists ? "Edit summary" : "Create summary…", exists ? Read(current, "text") : "",
            "Summary saved.", text => new
            {
                op = "save_summary", session_id = sessionId, expected_revision_id = expected, text,
            }, alternate: !exists && hasText ? generate : null, saveOperation: "save_summary");
        if (exists && hasText) content.Children.Add(generate);
        if (draft.ValueKind == JsonValueKind.Object)
        {
            if (draft.GetProperty("stale").GetBoolean()) AddLine("Generate a new draft from the updated transcript.");
            else
            {
                string draftId = Read(draft, "id");
                ShowTextEditor("Review generated draft", Read(draft, "text"), "Summary saved.", text => new
                {
                    op = "save_summary", session_id = sessionId, expected_revision_id = expected,
                    draft_id = draftId, text,
                }, saveOperation: "save_summary");
            }
        }
    }

    private void AddExport(string sessionId)
    {
        var button = new Button { Content = "Export note", HorizontalAlignment = HorizontalAlignment.Left };
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap };
        button.Click += async (_, _) =>
        {
            if (working || closed) return;
            if (MessageBox.Show(this, "Export creates an unencrypted file.", "Export note",
                    MessageBoxButton.OKCancel, MessageBoxImage.Information) != MessageBoxResult.OK) return;
            var path = new SaveFileDialog
            {
                FileName = "VoiceLedger-note",
                DefaultExt = ".txt",
                Filter = "Text (*.txt)|*.txt|Markdown (*.md)|*.md|JSON (*.json)|*.json",
            };
            if (path.ShowDialog(this) != true) return;
            string format = new[] { "text", "markdown", "json" }[path.FilterIndex - 1];
            button.IsEnabled = false;
            try
            {
                byte[] data = await Task.Run(() => NativeCore.SpeechRequest(
                    SpeechWorker.Request(new { op = "export", session_id = sessionId, format })));
                try { await File.WriteAllBytesAsync(path.FileName, data); }
                finally { Array.Clear(data); }
                status.Text = "Note exported.";
            }
            catch (Exception failure) when (failure is IOException or UnauthorizedAccessException
                or InvalidOperationException or TypeInitializationException)
            {
                status.Text = "Note export failed.";
            }
            finally { button.IsEnabled = true; }
        };
        content.Children.Add(button);
        content.Children.Add(status);
    }

    private void ShowTranslation(string sessionId, JsonElement line, StackPanel row, StackPanel edits)
    {
        JsonElement revision = line.GetProperty("translation");
        bool exists = revision.ValueKind == JsonValueKind.Object;
        string initial = exists ? Read(revision, "text") : "";
        if (exists)
        {
            string author = Read(revision, "authorship") == "manual" ? "Manual" : "Machine";
            string stale = revision.GetProperty("stale").GetBoolean() ? " · Outdated source" : "";
            AddLine($"Simplified Chinese · {author}{stale}", bold: true, panel: row);
            AddLine(initial, panel: row);
        }
        if (Read(line, "language") != "en") return;
        string utteranceId = Read(line, "utterance_id");
        string sourceRevision = Read(line, "revision_id");
        string? expected = exists ? Read(revision, "revision_id") : null;
        ShowTextEditor(exists ? "Edit translation" : "Add Chinese translation", initial, "Translation saved.",
            text => new
            {
                op = "save_translation", session_id = sessionId, utterance_id = utteranceId,
                source_revision_id = sourceRevision, expected_translation_id = expected, text,
            }, panel: edits);
    }

    private void ShowTextEditor(string label, string initial, string success, Func<string, object> action,
        int maxLength = 8192, bool allowBlank = false, StackPanel? panel = null, Button? alternate = null,
        string saveOperation = "save_translation")
    {
        panel ??= content;
        var edit = new Button
        {
            Content = label,
            HorizontalAlignment = HorizontalAlignment.Left,
            IsEnabled = canWork(),
        };
        var input = new TextBox
        {
            Text = initial, TextWrapping = TextWrapping.Wrap, AcceptsReturn = !allowBlank,
            MaxLength = maxLength, MinHeight = allowBlank ? 0 : 60, Visibility = Visibility.Collapsed,
        };
        AutomationProperties.SetName(input, label);
        var save = new Button
        {
            Content = "Save", HorizontalAlignment = HorizontalAlignment.Left,
            Visibility = Visibility.Collapsed, IsEnabled = allowBlank || !string.IsNullOrWhiteSpace(initial),
        };
        edit.Click += (_, _) =>
        {
            input.Visibility = Visibility.Visible;
            save.Visibility = Visibility.Visible;
            edit.Visibility = Visibility.Collapsed;
            if (alternate is not null) alternate.Visibility = Visibility.Visible;
            input.Focus();
        };
        var error = new TextBlock { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
        bool ValidText() => (allowBlank || !string.IsNullOrWhiteSpace(input.Text)) && Encoding.UTF8.GetByteCount(input.Text) <= 8192;
        input.TextChanged += (_, _) =>
        {
            save.IsEnabled = ValidText();
            if (alternate is not null) alternate.IsEnabled = canWork() && string.IsNullOrWhiteSpace(input.Text);
        };
        save.Click += async (_, _) =>
        {
            if (working || !canWork() || !ValidText()) return;
            working = true;
            save.IsEnabled = false;
            input.IsReadOnly = true;
            string request = SpeechWorker.Request(action(input.Text));
            int result = 0;
            try { await Task.Run(() => NativeCore.SpeechRequest(request)); }
            catch (CoreException failure) { result = failure.Code; }
            catch (Exception failure) when (failure is TypeInitializationException or EntryPointNotFoundException)
            {
                result = -31;
            }
            finally
            {
                working = false;
                save.IsEnabled = ValidText();
                input.IsReadOnly = false;
            }
            if (closed) return;
            if (result == 0) await LoadAsync(success);
            else
            {
                error.Text = ResultText(result, saveOperation);
                error.Visibility = Visibility.Visible;
            }
        };
        panel.Children.Add(edit);
        panel.Children.Add(input);
        panel.Children.Add(save);
        if (alternate is not null)
        {
            alternate.Visibility = Visibility.Collapsed;
            panel.Children.Add(alternate);
        }
        panel.Children.Add(error);
    }

    private async Task RunAsync(object action, string operation)
    {
        if (working || !canWork() || closed) return;
        working = true;
        cancelled = false;
        if (!sessions)
        {
            sessionAudio?.Dispose();
            sessionAudio = null;
            content.Children.Clear();
        }
        var progress = new StackPanel();
        AddLine(operation switch
        {
            "install" => "Downloading and verifying model…",
            "translate" => "Translating saved text…",
            "summarize" => "Generating summary…",
            "transcribe_external" => "Transcribing with external service…",
            _ => "Transcribing saved audio…",
        }, panel: progress);
        partial.Text = "";
        progress.Children.Add(partial);
        var cancel = new Button { Content = "Cancel", HorizontalAlignment = HorizontalAlignment.Left };
        cancel.Click += (_, _) => Cancel();
        progress.Children.Add(cancel);
        content.Children.Insert(sessions && content.Children.Count > 0 ? 1 : 0, progress);
        progress.BringIntoView();
        int result = -31;
        try
        {
            bool refreshed = operation is "transcribe_external" or "summarize"
                || await Task.Run(() => DeviceProfiler.Capture(SpeechWorker.ProfileDirectory));
            if (closed || cancelled || !canWork()) return;
            if (!refreshed)
            {
                result = -34;
                return;
            }
            using var cancellation = new CancellationTokenSource();
            worker = cancellation;
            result = await SpeechWorker.RunAsync(SpeechWorker.Request(action), line =>
            {
                if (!closed && !cancelled)
                {
                    using var snapshot = line.Length == 0 ? null : JsonDocument.Parse(line);
                    partial.Text = snapshot is not null && snapshot.RootElement.TryGetProperty("text", out var text)
                        ? text.GetString() ?? ""
                        : "";
                }
                return Task.CompletedTask;
            }, cancellation.Token);
        }
        finally
        {
            worker = null;
            working = false;
            progress.Children.Remove(partial);
            content.Children.Remove(progress);
            partial.Text = "";
            if (!closed)
            {
                await LoadAsync(cancelled ? "Speech task cancelled." : ResultText(result, operation));
            }
        }
    }

    private void Cancel()
    {
        cancelled = true;
        worker?.Cancel();
    }

    private static string ResultText(int result, string operation) => result switch
    {
        0 when operation == "install" => "Model verified.",
        0 when operation == "translate" => "Translation complete.",
        0 when operation == "summarize" => "Summary generated.",
        0 when operation == "transcribe_external" => "External transcription complete.",
        0 => "Saved audio processed.",
        -32 => "No committed audio is available in this session.",
        -33 => "Download the model first.",
        -34 => "Speech task paused by recording or device limits.",
        -35 => "Download interrupted. Retry to resume.",
        -36 => "Model checksum failed.",
        -37 => "Another speech task is running.",
        -38 when operation == "transcribe_external" => "External transcription paused. Send again to resume from saved progress.",
        -38 when operation == "summarize" => "Summary generation paused. Generate again when ready.",
        -38 => "Local task paused. Resume from saved progress.",
        -39 when operation is "summarize" or "save_summary" => "The summary or transcript changed. Refresh the note and try again.",
        -39 => "The source or translation changed. Reopen the editor with the current revision.",
        -40 => "Some lines exceeded the translation length limit. Completed translations were saved.",
        -41 => "Check the external service address, model and API key.",
        -42 => "The external service rejected the API key or permissions.",
        -43 => "The external service reached its rate limit. Try again later.",
        -44 => "Could not connect to the external service.",
        -45 when operation == "summarize" => "The external service did not return a complete summary.",
        -45 => "The external service could not transcribe the audio.",
        -46 => "No saved transcript is available.",
        -47 => "This transcript exceeds the summary generation limit.",
        _ when operation == "summarize" => "Summary generation stopped.",
        _ when operation == "save_summary" => "Could not save the summary.",
        _ when operation == "transcribe_external" => "External transcription stopped.",
        _ => "Local speech task stopped.",
    };

    private static string? JobStatus(JsonElement job) => Read(job, "state") switch
    {
        "running" => "Finishing transcription. Refresh the note to see new text.",
        "deferred" when Read(job, "reason") == "worker_interrupted" => "Transcription was interrupted. Resume from saved progress.",
        "deferred" => "Transcription paused. Resume from saved progress.",
        "failed" => "The speech worker failed. Saved progress can be retried.",
        _ => null,
    };

    private void AddHeading(string text, double size = 18) => content.Children.Add(new TextBlock
    {
        Text = text, FontSize = size, FontWeight = FontWeights.SemiBold,
        TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 14, 0, 6),
    });

    private void AddLine(string text, bool bold = false, StackPanel? panel = null, bool body = false)
    {
        (panel ?? content).Children.Add(new TextBlock
        {
            Text = text,
            TextWrapping = TextWrapping.Wrap,
            FontWeight = bold ? FontWeights.SemiBold : FontWeights.Normal,
            FontSize = body ? 15 : 12,
            Margin = new Thickness(0, bold ? 12 : 0, 0, 5),
        });
    }

    private static string Read(JsonElement json, string field) => json.GetProperty(field).GetString() ?? "";
    private static string Mib(long bytes) => $"{bytes / 1048576.0:F1} MiB";

    private static string Status(string? reason) => reason switch
    {
        null => "Compatible candidate",
        "device_profile" => "Device capabilities unavailable",
        "abi" or "os_version" or "cpu_features" => "Incompatible device",
        "storage_space" => "Insufficient storage",
        "storage_unknown" => "Available storage unknown",
        "memory_budget" => "Insufficient available RAM",
        "memory_unknown" => "Peak RAM or memory budget unknown",
        "runtime_unavailable" => "Local runtime unavailable",
        "memory_pressure" => "Low memory",
        "thermal_pressure" => "Thermal pressure",
        "battery_pressure" => "Battery pressure",
        _ => "Compatibility unconfirmed",
    };
}
