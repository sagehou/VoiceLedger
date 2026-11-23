using System;
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

    internal ModelsWindow()
    {
        Title = "Models";
        Width = 520;
        Height = 400;
        MinWidth = 320;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Content = new ScrollViewer { Content = content, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        AddLine("Loading models…");
        Loaded += async (_, _) => await LoadAsync();
    }

    private async Task LoadAsync()
    {
        try
        {
            using var report = await Task.Run(() =>
            {
                string directory = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "VoiceLedger");
                DeviceProfiler.Capture(directory);
                return JsonDocument.Parse(NativeCore.ModelPreview(directory));
            });
            content.Children.Clear();
            AddLine("Recording only", bold: true);
            foreach (JsonElement candidate in report.RootElement.GetProperty("candidates").EnumerateArray())
            {
                JsonElement model = candidate.GetProperty("model");
                JsonElement runtime = model.GetProperty("runtime");
                AddLine($"{Read(model, "name")} · {Read(model, "quantization")}", bold: true);
                AddLine($"{Read(runtime, "id")} {Read(runtime, "version")} · {Read(model.GetProperty("license"), "id")}");
                string languages = string.Join(", ", model.GetProperty("languages").EnumerateArray().Select(language => language.GetString()));
                AddLine($"Languages {languages} · Download {Mib(candidate.GetProperty("download_bytes").GetInt64())}");
                JsonElement ram = model.GetProperty("peak_ram_estimate");
                AddLine(ram.ValueKind == JsonValueKind.Null
                    ? "Peak RAM estimate unavailable"
                    : $"Estimated peak RAM {Mib(ram.GetProperty("bytes").GetInt64())}");
                AddLine($"Benchmark pending · {Status(candidate.GetProperty("blocked_by").GetString())}");
            }
        }
        catch (Exception failure) when (failure is InvalidOperationException or JsonException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            content.Children.Clear();
            AddLine("Model catalog unavailable. Recording only.");
        }
    }

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
        _ => "Compatibility unconfirmed",
    };
}
