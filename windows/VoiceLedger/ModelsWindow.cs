using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
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
        ItemsSource = new[] { "Auto", "Chinese", "English", "Japanese" }, SelectedIndex = 0,
    };
    private readonly bool sessions;
    private readonly TextBox search = new() { MaxLength = 64 };
    private string? selectedSession;
    private SessionAudioPanel? sessionAudio;
    private int searchVersion;
    private Process? worker;
    private bool cancelled;
    private bool closed;
    private bool working;

    internal ModelsWindow(Func<bool> canWork, bool sessions = false)
    {
        this.canWork = canWork;
        this.sessions = sessions;
        Title = sessions ? "Sessions" : "Models";
        Width = 520;
        Height = 400;
        MinWidth = 320;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Content = new ScrollViewer { Content = content, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        AddLine("Loading models…");
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
                var item = new Button
                {
                    Content = new TextBlock
                    {
                        Text = $"{name}{date.LocalDateTime:g} · {duration:g} · {state}\nAudio {saved} saved · {missing} missing · {transcript}",
                        TextWrapping = TextWrapping.Wrap,
                    },
                    Margin = new Thickness(0, 6, 0, 0),
                };
                item.Click += async (_, _) =>
                {
                    selectedSession = id;
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
            if (closed) return;
            using var report = JsonDocument.Parse(catalog);
            using var speech = JsonDocument.Parse(inspection);
            JsonElement session = speech.RootElement.GetProperty("session");
            string? sessionId = session.ValueKind == JsonValueKind.Null ? null : Read(session, "id");
            JsonElement lines = speech.RootElement.GetProperty("lines");
            bool hasEnglish = speech.RootElement.GetProperty("has_translation_source").GetBoolean();
            content.Children.Clear();
            if (sessions)
            {
                var back = new Button { Content = "Back to sessions", HorizontalAlignment = HorizontalAlignment.Left };
                back.Click += async (_, _) =>
                {
                    selectedSession = null;
                    await LoadAsync();
                };
                content.Children.Add(back);
            }
            if (status is not null) AddLine(status);
            else if (speech.RootElement.GetProperty("job") is { ValueKind: JsonValueKind.Object } job
                && JobStatus(job) is { } text) AddLine(text);
            if (sessionId is not null)
            {
                var created = DateTimeOffset.FromUnixTimeMilliseconds(session.GetProperty("created_at_ms").GetInt64());
                AddLine($"{created.LocalDateTime:g}");
                AddLine("Transcription language");
                content.Children.Add(language);
                if (sessions)
                {
                    string title = Read(speech.RootElement, "title");
                    if (title.Length > 0) AddLine(title, bold: true);
                    ShowTextEditor("Rename session", title, "Title saved.", text => new
                    {
                        op = "rename_session", session_id = sessionId, text,
                    }, maxLength: 64, allowBlank: true);
                    if (lines.GetArrayLength() > 0) AddExport(sessionId);
                    sessionAudio = new SessionAudioPanel(sessionId, speech.RootElement.GetProperty("tracks"), canWork);
                    content.Children.Add(sessionAudio);
                }
            }
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
                AddLine($"{Read(model, "name")} · {Read(model, "quantization")}", bold: true);
                AddLine($"{Read(runtime, "id")} {Read(runtime, "version")} · {Read(model.GetProperty("license"), "id")}");
                string languages = string.Join(", ", model.GetProperty("languages").EnumerateArray().Select(language => language.GetString()));
                AddLine($"Languages {languages} · Download {Mib(candidate.GetProperty("download_bytes").GetInt64())}");
                JsonElement ram = model.GetProperty("peak_ram_estimate");
                AddLine(ram.ValueKind == JsonValueKind.Null
                    ? "Peak RAM estimate unavailable"
                    : $"Estimated peak RAM {Mib(ram.GetProperty("bytes").GetInt64())}");
                AddLine($"Benchmark pending · {Status(blockedBy)}");
                var action = new Button
                {
                    Content = !installed ? "Download and verify"
                        : translation ? "Translate English transcript" : "Transcribe saved session",
                    HorizontalAlignment = HorizontalAlignment.Left,
                    Padding = new Thickness(10, 4, 10, 4),
                    IsEnabled = blockedBy is null && canWork()
                        && (!installed || (sessionId is not null && (!translation || hasEnglish))),
                };
                action.Click += async (_, _) =>
                {
                    var policy = new[] { "auto", "zh", "en", "ja" }[Math.Max(0, language.SelectedIndex)];
                    string operation = !installed ? "install" : translation ? "translate" : "transcribe";
                    object request = operation switch
                    {
                        "install" => new { op = operation, model_id = id, approved = true },
                        "translate" => new { op = operation, model_id = id, session_id = sessionId, approved = true },
                        _ => new { op = operation, model_id = id, session_id = sessionId, language = policy, approved = true },
                    };
                    await RunAsync(request, operation);
                };
                content.Children.Add(action);
                if (translation && installed && !hasEnglish)
                    action.ToolTip = "Transcribe an English recording first.";
            }
            var transcriptRows = new List<(string Source, UIElement Element)>();
            foreach (JsonElement line in lines.EnumerateArray())
            {
                int first = content.Children.Count;
                string sourceId = Read(line, "source");
                string source = sourceId == "mic" ? "Microphone" : "System loopback";
                AddLine($"{source} · {line.GetProperty("start_ns").GetInt64() / 1_000_000_000.0:F1}s", bold: true);
                AddLine(Read(line, "text"));
                if (sessionId is not null)
                {
                    string utterance = Read(line, "utterance_id");
                    string revision = Read(line, "revision_id");
                    ShowTextEditor("Edit transcript", Read(line, "text"), "Transcript saved.", text => new
                    {
                        op = "save_utterance", session_id = sessionId, utterance_id = utterance,
                        expected_revision_id = revision, text,
                    });
                    ShowTranslation(sessionId, line);
                    if (sessions)
                    {
                        JsonElement speaker = line.GetProperty("speaker");
                        bool labeled = speaker.ValueKind == JsonValueKind.Object;
                        string name = labeled ? Read(speaker, "name") : "";
                        AddLine(name.Length > 0 ? name : "Unknown speaker");
                        ShowTextEditor("Edit speaker name", name, "Speaker saved.", text => new
                        {
                            op = "save_speaker", session_id = sessionId, utterance_id = utterance,
                            expected_revision_id = revision,
                            expected_speaker_id = labeled ? Read(speaker, "revision_id") : null,
                            text,
                        }, maxLength: 64, allowBlank: true);
                    }
                }
                if (sessionAudio is not null)
                {
                    var group = new StackPanel();
                    while (content.Children.Count > first)
                    {
                        UIElement element = content.Children[first];
                        content.Children.RemoveAt(first);
                        group.Children.Add(element);
                    }
                    content.Children.Add(group);
                    transcriptRows.Add((sourceId, group));
                }
            }
            if (sessionAudio is not null)
                sessionAudio.TrackChanged += source =>
                {
                    foreach (var row in transcriptRows)
                        row.Element.Visibility = source is null || row.Source == source
                            ? Visibility.Visible : Visibility.Collapsed;
                };
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            content.Children.Clear();
            AddLine("Model catalog unavailable. Recording only.");
        }
    }

    private void AddExport(string sessionId)
    {
        var button = new Button { Content = "Export transcript", HorizontalAlignment = HorizontalAlignment.Left };
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap };
        button.Click += async (_, _) =>
        {
            if (working || closed) return;
            if (MessageBox.Show(this, "Export creates an unencrypted file.", "Export transcript",
                    MessageBoxButton.OKCancel, MessageBoxImage.Information) != MessageBoxResult.OK) return;
            var path = new SaveFileDialog
            {
                FileName = "VoiceLedger-transcript",
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
                status.Text = "Transcript exported.";
            }
            catch (Exception failure) when (failure is IOException or UnauthorizedAccessException
                or InvalidOperationException or TypeInitializationException)
            {
                status.Text = "Transcript export failed.";
            }
            finally { button.IsEnabled = true; }
        };
        content.Children.Add(button);
        content.Children.Add(status);
    }

    private void ShowTranslation(string sessionId, JsonElement line)
    {
        JsonElement revision = line.GetProperty("translation");
        bool exists = revision.ValueKind == JsonValueKind.Object;
        string initial = exists ? Read(revision, "text") : "";
        if (exists)
        {
            string author = Read(revision, "authorship") == "manual" ? "Manual" : "Machine";
            string stale = revision.GetProperty("stale").GetBoolean() ? " · Outdated source" : "";
            AddLine($"Simplified Chinese · {author}{stale}", bold: true);
            AddLine(initial);
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
            });
    }

    private void ShowTextEditor(string label, string initial, string success, Func<string, object> action,
        int maxLength = 8192, bool allowBlank = false)
    {
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
            input.Focus();
        };
        var error = new TextBlock { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
        bool ValidText() => (allowBlank || !string.IsNullOrWhiteSpace(input.Text)) && Encoding.UTF8.GetByteCount(input.Text) <= 8192;
        input.TextChanged += (_, _) => save.IsEnabled = ValidText();
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
                error.Text = ResultText(result, "save_translation");
                error.Visibility = Visibility.Visible;
            }
        };
        content.Children.Add(edit);
        content.Children.Add(input);
        content.Children.Add(save);
        content.Children.Add(error);
    }

    private async Task RunAsync(object action, string operation)
    {
        if (working || !canWork() || closed) return;
        working = true;
        cancelled = false;
        sessionAudio?.Dispose();
        sessionAudio = null;
        content.Children.Clear();
        AddLine(operation switch
        {
            "install" => "Downloading and verifying model…",
            "translate" => "Translating saved text…",
            _ => "Transcribing saved audio…",
        });
        partial.Text = "";
        content.Children.Add(partial);
        var cancel = new Button { Content = "Cancel", HorizontalAlignment = HorizontalAlignment.Left };
        cancel.Click += (_, _) => Cancel();
        content.Children.Add(cancel);
        int result = -31;
        try
        {
            bool refreshed = await Task.Run(() => DeviceProfiler.Capture(SpeechWorker.ProfileDirectory));
            if (closed || cancelled || !canWork()) return;
            if (!refreshed)
            {
                result = -34;
                return;
            }
            string name = $"voiceledger-speech-{Guid.NewGuid():N}";
            using var pipe = new NamedPipeServerStream(name, PipeDirection.In, 1, PipeTransmissionMode.Byte,
                PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
            using var receiveCancelled = new CancellationTokenSource();
            using var process = new Process { StartInfo = SpeechWorker.StartInfo(SpeechWorker.Request(action), name) };
            if (!process.Start()) return;
            worker = process;
            try { process.PriorityClass = ProcessPriorityClass.BelowNormal; }
            catch (System.ComponentModel.Win32Exception) { }
            catch (InvalidOperationException) { }
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            Task receive = ReceivePartialsAsync(pipe, receiveCancelled.Token);
            try { await process.WaitForExitAsync(); }
            finally
            {
                await receiveCancelled.CancelAsync();
                await receive;
            }
            result = process.ExitCode;
        }
        catch (Exception failure) when (failure is InvalidOperationException or IOException
            or System.ComponentModel.Win32Exception)
        {
        }
        finally
        {
            worker = null;
            working = false;
            partial.Text = "";
            if (!closed)
            {
                await LoadAsync(cancelled ? "Local task cancelled." : ResultText(result, operation));
            }
        }
    }

    private async Task ReceivePartialsAsync(NamedPipeServerStream pipe, CancellationToken cancellation)
    {
        try
        {
            await pipe.WaitForConnectionAsync(cancellation);
            using var reader = new StreamReader(pipe);
            while ((await reader.ReadLineAsync(cancellation)) is { } line)
            {
                if (closed || cancelled) return;
                using var snapshot = line.Length == 0 ? null : JsonDocument.Parse(line);
                partial.Text = snapshot is null ? "" : Read(snapshot.RootElement, "text");
            }
        }
        catch (Exception failure) when (failure is OperationCanceledException or IOException or JsonException)
        {
        }
    }

    private void Cancel()
    {
        cancelled = true;
        try
        {
            if (worker is { HasExited: false } active) active.Kill(entireProcessTree: true);
        }
        catch (Exception failure) when (failure is InvalidOperationException or System.ComponentModel.Win32Exception)
        {
        }
    }

    private static string ResultText(int result, string operation) => result switch
    {
        0 when operation == "install" => "Model verified.",
        0 when operation == "translate" => "Translation complete.",
        0 => "Saved audio processed.",
        -32 => "No committed audio is available in this session.",
        -33 => "Download the model first.",
        -34 => "Speech task paused by recording or device limits.",
        -35 => "Download interrupted. Retry to resume.",
        -36 => "Model checksum failed.",
        -37 => "Another local speech task is running.",
        -38 => "Local task paused. Resume from saved progress.",
        -39 => "The source or translation changed. Reopen the editor with the current revision.",
        -40 => "Some lines exceeded the translation length limit. Completed translations were saved.",
        _ => "Local speech task stopped.",
    };

    private static string? JobStatus(JsonElement job) => Read(job, "state") switch
    {
        "deferred" when Read(job, "reason") == "worker_interrupted" => "Transcription was interrupted. Resume from saved progress.",
        "deferred" => "Transcription paused. Resume from saved progress.",
        "failed" => "The speech worker failed. Saved progress can be retried.",
        _ => null,
    };

    private void AddLine(string text, bool bold = false)
    {
        content.Children.Add(new TextBlock
        {
            Text = text,
            TextWrapping = TextWrapping.Wrap,
            FontWeight = bold ? FontWeights.SemiBold : FontWeights.Normal,
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
