using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
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
    private Process? worker;
    private bool cancelled;
    private bool closed;
    private bool working;

    internal ModelsWindow(Func<bool> canWork)
    {
        this.canWork = canWork;
        Title = "Models";
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
        Cancel();
        base.OnClosed(e);
    }

    private async Task LoadAsync(string? status = null)
    {
        try
        {
            var (catalog, inspection) = await Task.Run(() =>
            {
                string directory = SpeechWorker.ProfileDirectory;
                DeviceProfiler.Capture(directory);
                return (NativeCore.ModelPreview(directory), NativeCore.SpeechRequest(SpeechWorker.Request(new { op = "inspect" })));
            });
            if (closed) return;
            using var report = JsonDocument.Parse(catalog);
            using var speech = JsonDocument.Parse(inspection);
            JsonElement session = speech.RootElement.GetProperty("session");
            string? sessionId = session.ValueKind == JsonValueKind.Null ? null : Read(session, "id");
            content.Children.Clear();
            AddLine("Recording only during capture", bold: true);
            if (status is not null) AddLine(status);
            if (sessionId is not null)
            {
                var created = DateTimeOffset.FromUnixTimeMilliseconds(session.GetProperty("created_at_ms").GetInt64());
                AddLine($"Latest saved session · {created.LocalDateTime:g}");
                AddLine("Transcription language");
                content.Children.Add(language);
            }
            foreach (JsonElement candidate in report.RootElement.GetProperty("candidates").EnumerateArray())
            {
                JsonElement model = candidate.GetProperty("model");
                JsonElement runtime = model.GetProperty("runtime");
                string id = Read(model, "id");
                JsonElement choice = speech.RootElement.GetProperty("models").EnumerateArray()
                    .First(option => Read(option, "id") == id);
                bool installed = choice.GetProperty("installed").GetBoolean();
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
                    Content = installed ? "Transcribe latest saved session" : "Download and verify",
                    HorizontalAlignment = HorizontalAlignment.Left,
                    Padding = new Thickness(10, 4, 10, 4),
                    IsEnabled = blockedBy is null && canWork() && (!installed || sessionId is not null),
                };
                action.Click += async (_, _) => await RunAsync(id, installed ? sessionId : null);
                content.Children.Add(action);
            }
            foreach (JsonElement line in speech.RootElement.GetProperty("lines").EnumerateArray())
            {
                string source = Read(line, "source") == "mic" ? "Microphone" : "System loopback";
                AddLine($"{source} · {line.GetProperty("start_ns").GetInt64() / 1_000_000_000.0:F1}s", bold: true);
                AddLine(Read(line, "text"));
            }
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            content.Children.Clear();
            AddLine("Model catalog unavailable. Recording only.");
        }
    }

    private async Task RunAsync(string modelId, string? sessionId)
    {
        if (working || !canWork() || closed) return;
        working = true;
        cancelled = false;
        var policy = new[] { "auto", "zh", "en", "ja" }[Math.Max(0, language.SelectedIndex)];
        object action = sessionId is null
            ? new { op = "install", model_id = modelId, approved = true }
            : new { op = "transcribe", model_id = modelId, session_id = sessionId, language = policy, approved = true };
        content.Children.Clear();
        AddLine(sessionId is null ? "Downloading and verifying model…" : "Transcribing saved audio…");
        partial.Text = "";
        content.Children.Add(partial);
        var cancel = new Button { Content = "Cancel", HorizontalAlignment = HorizontalAlignment.Left };
        cancel.Click += (_, _) => Cancel();
        content.Children.Add(cancel);
        int result = -31;
        try
        {
            await Task.Run(() => DeviceProfiler.Capture(SpeechWorker.ProfileDirectory));
            if (closed || cancelled || !canWork()) return;
            using var process = new Process { StartInfo = SpeechWorker.StartInfo(SpeechWorker.Request(action)) };
            process.OutputDataReceived += (_, args) =>
            {
                const string prefix = "VL_PARTIAL ";
                if (args.Data is not { } line || !line.StartsWith(prefix, StringComparison.Ordinal)) return;
                try
                {
                    byte[] bytes = Convert.FromBase64String(line[prefix.Length..]);
                    using var snapshot = bytes.Length == 0 ? null : JsonDocument.Parse(bytes);
                    string text = snapshot is null ? "" : Read(snapshot.RootElement, "text");
                    Dispatcher.InvokeAsync(() =>
                    {
                        if (worker == process && !closed && !cancelled) partial.Text = text;
                    });
                }
                catch (Exception failure) when (failure is FormatException or JsonException or InvalidOperationException)
                {
                }
            };
            process.ErrorDataReceived += (_, _) => { };
            if (!process.Start()) return;
            worker = process;
            try { process.PriorityClass = ProcessPriorityClass.BelowNormal; }
            catch (System.ComponentModel.Win32Exception) { }
            catch (InvalidOperationException) { }
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            await process.WaitForExitAsync();
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
                await LoadAsync(cancelled ? "Speech task cancelled." : ResultText(result, sessionId is null));
            }
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

    private static string ResultText(int result, bool install) => result switch
    {
        0 => install ? "Model verified." : "Saved audio processed.",
        -32 => "No committed audio is available in this session.",
        -33 => "Download the model first.",
        -34 => "Speech task paused by recording or device limits.",
        -35 => "Download interrupted. Retry to resume.",
        -36 => "Model checksum failed.",
        _ => "Local speech task stopped.",
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
