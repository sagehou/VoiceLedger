using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed record LiveLine(string Caption, string Text, string? Translation, bool IsPartial);

internal static class LiveTranscript
{
    internal static string ModelStatus()
    {
        using var report = JsonDocument.Parse(NativeCore.SpeechRequest(SpeechWorker.Request(new { op = "live_model" })));
        return Text(report.RootElement, "status");
    }

    internal static (LiveLine[] Lines, bool TranslationPending, string? Stage, string? Reason) Read(
        string request, string progress, IReadOnlyDictionary<Guid, string> sources)
    {
        using var snapshot = JsonDocument.Parse(NativeCore.SpeechRequest(request));
        JsonElement report = snapshot.RootElement;
        var lines = report.GetProperty("lines").EnumerateArray().Reverse().Select(line =>
        {
            string track = line.GetProperty("source").GetString() == "mic" ? "Microphone" : "System loopback";
            var translation = line.GetProperty("translation");
            string? translated = translation.ValueKind == JsonValueKind.Object
                ? $"Simplified Chinese{(translation.GetProperty("stale").GetBoolean() ? " (outdated)" : "")} · {Text(translation, "text")}"
                : null;
            return new LiveLine($"{track} · {SpeechWorker.LanguageName(Text(line, "language"))}",
                Text(line, "text"), translated, false);
        }).ToList();
        string? stage = null;
        if (progress.Length != 0)
        {
            using var current = JsonDocument.Parse(progress);
            stage = Text(current.RootElement, "stage");
            foreach (var partial in current.RootElement.GetProperty("partials").EnumerateArray())
            {
                if (partial.GetProperty("track_id").TryGetGuid(out Guid id) && sources.TryGetValue(id, out string? track))
                    lines.Insert(0, new LiveLine($"{track} · {SpeechWorker.LanguageName(Text(partial, "language"))} · Partial",
                        Text(partial, "text"), null, true));
            }
        }
        var job = report.GetProperty("job");
        string? reason = job.ValueKind == JsonValueKind.Object ? job.GetProperty("reason").GetString() : null;
        return (lines.ToArray(), report.GetProperty("translation_pending").GetBoolean(), stage, reason);
    }

    private static string Text(JsonElement value, string field) => value.GetProperty(field).GetString() ?? "";
}
