using System;
using System.ComponentModel;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using System.Threading.Tasks;
using Microsoft.Win32;
using Forms = System.Windows.Forms;

namespace VoiceLedger;

public partial class MainWindow : Window
{
    private readonly RecordingViewModel viewModel = new();
    private readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromMilliseconds(250) };
    private readonly Forms.NotifyIcon tray = new() { Icon = System.Drawing.SystemIcons.Application, Text = "VoiceLedger" };
    private readonly Forms.ToolStripMenuItem trayStop = new("Stop recording");
    private bool closePromptPending;

    public MainWindow()
    {
        InitializeComponent();
        DataContext = viewModel;
        var menu = new Forms.ContextMenuStrip();
        var show = new Forms.ToolStripMenuItem("Show VoiceLedger");
        show.Click += (_, _) => Dispatcher.InvokeAsync(ShowFromTray);
        trayStop.Click += (_, _) => Dispatcher.InvokeAsync(() => { _ = viewModel.StopAsync(); });
        menu.Items.Add(show);
        menu.Items.Add(trayStop);
        tray.ContextMenuStrip = menu;
        tray.DoubleClick += (_, _) => Dispatcher.InvokeAsync(ShowFromTray);

        viewModel.PropertyChanged += (_, _) => UpdateTray();
        timer.Tick += async (_, _) => await viewModel.RefreshRecordingAsync();
        Loaded += async (_, _) =>
        {
            await viewModel.InitializeAsync(initial: true);
            timer.Start();
        };
        SystemEvents.PowerModeChanged += PowerModeChanged;
    }

    private async void RefreshDevices(object sender, RoutedEventArgs e) => await viewModel.InitializeAsync(initial: false);
    private async void StartRecording(object sender, RoutedEventArgs e) => await viewModel.StartAsync();
    private async void PauseRecording(object sender, RoutedEventArgs e) => await viewModel.PauseAsync();
    private async void ResumeRecording(object sender, RoutedEventArgs e) => await viewModel.ResumeAsync();
    private async void StopRecording(object sender, RoutedEventArgs e) => await viewModel.StopAsync();
    private void OpenRecordingFolder(object sender, RoutedEventArgs e) => viewModel.OpenFolder();
    private async void PlaySavedAudio(object sender, RoutedEventArgs e) => await viewModel.PlayAsync();
    private void StopPlayback(object sender, RoutedEventArgs e) => viewModel.StopPlayback();
    private void OpenSessions(object sender, RoutedEventArgs e) =>
        new ModelsWindow(() => !viewModel.Active && !viewModel.Busy, sessions: true) { Owner = this }.ShowDialog();
    private void OpenSavedNote(object sender, RoutedEventArgs e)
    {
        if (viewModel.SavedNoteId is { } sessionId)
            new ModelsWindow(() => viewModel.CanSelect, sessions: true, initialSession: sessionId) { Owner = this }.ShowDialog();
    }

    private async void OpenModels(object sender, RoutedEventArgs e)
    {
        new ModelsWindow(() => !viewModel.Active && !viewModel.Busy) { Owner = this }.ShowDialog();
        await viewModel.RefreshLiveModelAsync();
    }

    private void OpenSyncPrivacy(object sender, RoutedEventArgs e) =>
        new SyncPrivacyWindow { Owner = this }.ShowDialog();

    private async void ReconnectEndpoint(object sender, RoutedEventArgs e)
    {
        if (sender is Button { DataContext: TrackRow row }) await viewModel.ReconnectAsync(row.Id);
    }

    private void PowerModeChanged(object sender, PowerModeChangedEventArgs e)
    {
        if (e.Mode == PowerModes.Suspend) viewModel.Suspend();
    }

    private void ShowFromTray()
    {
        Show();
        WindowState = WindowState.Normal;
        Activate();
        UpdateTray();
    }

    private void UpdateTray()
    {
        tray.Visible = viewModel.Active || !IsVisible;
        trayStop.Enabled = viewModel.CanStop;
    }

    protected override void OnClosing(CancelEventArgs e)
    {
        base.OnClosing(e);
        if (e.Cancel || viewModel.Busy)
        {
            e.Cancel = true;
            return;
        }
        if (!viewModel.Active) return;
        e.Cancel = true;
        if (!closePromptPending)
        {
            closePromptPending = true;
            Dispatcher.InvokeAsync(() => { _ = ChooseCloseActionAsync(); });
        }
    }

    private async Task ChooseCloseActionAsync()
    {
        try
        {
            switch (PromptWhileRecording())
            {
                case CloseAction.Background:
                    Hide();
                    UpdateTray();
                    break;
                case CloseAction.StopAndClose:
                    await viewModel.StopAsync();
                    if (!viewModel.Active) Close();
                    break;
            }
        }
        finally
        {
            closePromptPending = false;
        }
    }

    protected override async void OnClosed(EventArgs e)
    {
        timer.Stop();
        SystemEvents.PowerModeChanged -= PowerModeChanged;
        tray.ContextMenuStrip?.Dispose();
        tray.Dispose();
        await viewModel.DisposeAsync();
        base.OnClosed(e);
    }

    private CloseAction PromptWhileRecording()
    {
        var dialog = new Window
        {
            Owner = this, Title = "Recording is active", WindowStartupLocation = WindowStartupLocation.CenterOwner,
            ResizeMode = ResizeMode.NoResize, SizeToContent = SizeToContent.WidthAndHeight, ShowInTaskbar = false
        };
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(16) };
        var result = CloseAction.Cancel;
        AddButton("Continue in background", CloseAction.Background);
        AddButton("Stop and close", CloseAction.StopAndClose);
        AddButton("Cancel", CloseAction.Cancel);
        dialog.Content = actions;
        dialog.ShowDialog();
        return result;

        void AddButton(string label, CloseAction action)
        {
            var button = new Button { Content = label, Margin = new Thickness(4), Padding = new Thickness(10, 5, 10, 5),
                IsCancel = action == CloseAction.Cancel };
            button.Click += (_, _) => { result = action; dialog.Close(); };
            actions.Children.Add(button);
        }
    }

    private enum CloseAction { Cancel, Background, StopAndClose }
}
