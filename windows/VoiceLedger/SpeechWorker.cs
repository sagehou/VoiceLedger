using System;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
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

    private static ProcessStartInfo StartInfo(string request, string pipe, string executable)
    {
        var info = new ProcessStartInfo(executable)
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
        };
        info.ArgumentList.Add("--speech-worker");
        info.ArgumentList.Add(request);
        info.ArgumentList.Add(pipe);
        return info;
    }

    internal static string[] Languages { get; } = ["Auto", "Chinese", "English", "Japanese"];
    internal static string LanguageCode(int index) => index switch { 1 => "zh", 2 => "en", 3 => "ja", _ => "auto" };
    internal static string LanguageName(string code) => code switch
    {
        "zh" => "Chinese",
        "ja" => "Japanese",
        "en" => "English",
        _ => code,
    };

    internal static async Task<int> RunAsync(string request, Func<string, Task> update, CancellationToken cancellation,
        string? executable = null)
    {
        try
        {
            cancellation.ThrowIfCancellationRequested();
            string name = $"voiceledger-speech-{Guid.NewGuid():N}";
            using var pipe = new NamedPipeServerStream(name, PipeDirection.In, 1, PipeTransmissionMode.Byte,
                PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
            using var receiving = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            using var process = new Process { StartInfo = StartInfo(request, name, executable ?? Path.Combine(AppContext.BaseDirectory, "VoiceLedger.exe")) };
            if (!process.Start()) return -31;
            Task receive = Task.CompletedTask;
            try
            {
                try { process.PriorityClass = ProcessPriorityClass.BelowNormal; }
                catch (System.ComponentModel.Win32Exception) { }
                catch (InvalidOperationException) { }
                process.BeginOutputReadLine();
                process.BeginErrorReadLine();
                receive = ReceiveAsync(pipe, update, receiving.Token);
                await process.WaitForExitAsync(cancellation);
                return process.ExitCode;
            }
            finally
            {
                try
                {
                    if (!process.HasExited) process.Kill(entireProcessTree: true);
                }
                catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException) { }
                await receiving.CancelAsync();
                await receive;
            }
        }
        catch (OperationCanceledException) { return -4; }
        catch (Exception error) when (error is IOException or InvalidOperationException
            or System.ComponentModel.Win32Exception or UnauthorizedAccessException)
        {
            return -31;
        }
    }

    private static async Task ReceiveAsync(NamedPipeServerStream pipe, Func<string, Task> update, CancellationToken cancellation)
    {
        try
        {
            await pipe.WaitForConnectionAsync(cancellation);
            using var reader = new StreamReader(pipe);
            while ((await reader.ReadLineAsync(cancellation)) is { } line)
                await update(line);
        }
        catch (Exception error) when (error is IOException or JsonException or OperationCanceledException) { }
    }

    internal static int Run(string request, string? pipeName)
    {
        try
        {
            using var pipe = pipeName is null ? null : new NamedPipeClientStream(
                ".", pipeName, PipeDirection.Out, PipeOptions.CurrentUserOnly);
            pipe?.Connect(2000);
            using var writer = pipe is null ? null : new StreamWriter(pipe, new UTF8Encoding(false)) { AutoFlush = true };
            string directory = ProfileDirectory;
            if (pipe is not null)
            {
                using var command = JsonDocument.Parse(request);
                directory = command.RootElement.GetProperty("profiles").GetString() ?? ProfileDirectory;
                NativeCore.UpdateSpeechResources(DeviceProfiler.SpeechResources(directory));
            }
            var sampling = new object();
            using var progress = writer is null ? null : new Timer(_ =>
            {
                if (!Monitor.TryEnter(sampling)) return;
                try
                {
                    NativeCore.UpdateSpeechResources(DeviceProfiler.SpeechResources(directory));
                    writer.WriteLine(Encoding.UTF8.GetString(NativeCore.SpeechPartial()));
                }
                catch (IOException)
                {
                    NativeCore.CancelSpeech();
                }
                catch (Exception error) when (error is InvalidOperationException
                    or TypeInitializationException or EntryPointNotFoundException) { }
                finally { Monitor.Exit(sampling); }
            }, null, 500, 500);
            _ = NativeCore.SpeechRequest(request);
            return 0;
        }
        catch (CoreException error)
        {
            return error.Code;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException or TimeoutException
            or TypeInitializationException or EntryPointNotFoundException or UnauthorizedAccessException or JsonException)
        {
            return -31;
        }
    }
}
