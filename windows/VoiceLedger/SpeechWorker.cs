using System;
using System.Diagnostics;
using System.IO;
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

    internal static ProcessStartInfo StartInfo(string request)
    {
        var info = new ProcessStartInfo(Path.Combine(AppContext.BaseDirectory, "VoiceLedger.exe"))
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
        };
        info.ArgumentList.Add("--speech-worker");
        info.ArgumentList.Add(request);
        return info;
    }

    internal static int Run(string request)
    {
        using var progress = new Timer(_ =>
        {
            try
            {
                // This stream is an app-private pipe owned by the parent process.
                Console.Out.WriteLine("VL_PARTIAL " + Convert.ToBase64String(NativeCore.SpeechPartial()));
            }
            catch (Exception error) when (error is IOException or InvalidOperationException
                or TypeInitializationException or EntryPointNotFoundException)
            {
            }
        }, null, 500, 500);

        try
        {
            _ = NativeCore.SpeechRequest(request);
            return 0;
        }
        catch (CoreException error)
        {
            return error.Code;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException
            or TypeInitializationException or EntryPointNotFoundException)
        {
            return -31;
        }
    }
}
