using System;
using System.Diagnostics;
using System.IO;
using System.Threading.Tasks;
using System.Windows;
using VoiceLedger.Core;

namespace VoiceLedger;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        if (e.Args is ["--speech-worker", var request])
        {
            StartupUri = null!;
            Shutdown(SpeechWorker.Run(request));
            return;
        }
        base.OnStartup(e);
        _ = Task.Run(() =>
        {
            string directory = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "VoiceLedger");
            if (!DeviceProfiler.Capture(directory)) Trace.TraceWarning("Device profile unavailable.");
        });
    }
}
