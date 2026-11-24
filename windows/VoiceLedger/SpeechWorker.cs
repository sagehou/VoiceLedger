using System;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Threading;
using VoiceLedger.Core;

namespace VoiceLedger;

internal static class SpeechWorker
{
    internal static string ProfileDirectory { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "VoiceLedger");
    internal static string Request(object action) => JsonSerializer.Serialize(new
    {
        recordings = Path.Combine(ProfileDirectory, "Recordings"),
        profiles = ProfileDirectory,
        adapters = Path.Combine(AppContext.BaseDirectory, "speech"),
        action,
    });

    internal static ProcessStartInfo StartInfo(string request, string pipe)
    {
        var info = new ProcessStartInfo(Path.Combine(AppContext.BaseDirectory, "VoiceLedger.exe"))
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
        };
        info.ArgumentList.Add("--speech-worker");
        info.ArgumentList.Add(request);
        info.ArgumentList.Add(pipe);
        return info;
    }

    internal static int Run(string request, string? pipeName)
    {
        try
        {
            using var pipe = pipeName is null ? null : new NamedPipeClientStream(
                ".", pipeName, PipeDirection.Out, PipeOptions.CurrentUserOnly);
            pipe?.Connect(2000);
            using var writer = pipe is null ? null : new StreamWriter(pipe, new UTF8Encoding(false)) { AutoFlush = true };
            using var progress = writer is null ? null : new Timer(_ =>
            {
                try
                {
                    writer.WriteLine(Encoding.UTF8.GetString(NativeCore.SpeechPartial()));
                }
                catch (Exception error) when (error is IOException or InvalidOperationException
                    or TypeInitializationException or EntryPointNotFoundException)
                {
                }
            }, null, 500, 500);
            _ = NativeCore.SpeechRequest(request);
            return 0;
        }
        catch (CoreException error)
        {
            return error.Code;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException or TimeoutException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            return -31;
        }
    }
}
