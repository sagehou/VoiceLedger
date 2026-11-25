using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed record LiveLine(string Caption, string Text, string? Translation, bool IsPartial);

internal static class LiveTranscript
{
    internal static (LiveLine[] Lines, bool TranslationPending) Read(
        string request, string progress, IReadOnlyDictionary<Guid, string> sources)
    {
        using var snapshot = JsonDocument.Parse(NativeCore.SpeechRequest(request));
        var lines = snapshot.RootElement.GetProperty("lines").EnumerateArray().Select(line =>
        {
            string track = line.GetProperty("source").GetString() == "mic" ? "Microphone" : "System loopback";
            var translation = line.GetProperty("translation");
            string? translated = translation.ValueKind == JsonValueKind.Object
                ? $"Simplified Chinese{(translation.GetProperty("stale").GetBoolean() ? " (outdated)" : "")} · {Text(translation, "text")}"
                : null;
            return new LiveLine($"{track} · {SpeechWorker.LanguageName(Text(line, "language"))}",
                Text(line, "text"), translated, false);
        }).ToList();
        if (progress.Length != 0)
        {
            using var current = JsonDocument.Parse(progress);
            foreach (var partial in current.RootElement.GetProperty("partials").EnumerateArray())
            {
                if (partial.GetProperty("track_id").TryGetGuid(out Guid id) && sources.TryGetValue(id, out string? track))
                    lines.Add(new LiveLine($"{track} · {SpeechWorker.LanguageName(Text(partial, "language"))} · Partial",
                        Text(partial, "text"), null, true));
            }
        }
        return (lines.ToArray(), snapshot.RootElement.GetProperty("translation_pending").GetBoolean());
    }

    private static string Text(JsonElement value, string field) => value.GetProperty(field).GetString() ?? "";
}
