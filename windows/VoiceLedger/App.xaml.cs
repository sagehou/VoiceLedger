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
        if (e.Args.Length is 2 or 3 && e.Args[0] == "--speech-worker")
        {
            Shutdown(SpeechWorker.Run(e.Args[1], e.Args.Length == 3 ? e.Args[2] : null));
            return;
        }
        base.OnStartup(e);
        MainWindow = new MainWindow();
        MainWindow.Show();
        _ = Task.Run(() =>
        {
            string directory = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "VoiceLedger");
            if (!DeviceProfiler.Capture(directory)) Trace.TraceWarning("Device profile unavailable.");
        });
    }
}
