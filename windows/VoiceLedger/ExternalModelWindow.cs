using System;
using System.Linq;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace VoiceLedger;

internal sealed class ExternalModelWindow : Window
{
    private readonly StackPanel fields = new() { Margin = new Thickness(18) };

    internal object? Request { get; private set; }

    internal ExternalModelWindow(Window owner, string sessionId, JsonElement presets,
        JsonElement? savedTracks = null, int sourceLanguage = 0)
    {
        bool summary = savedTracks is null;
        Owner = owner;
        Title = summary ? "Generate summary" : "External transcription";
        Width = 460;
        SizeToContent = SizeToContent.Height;
        MaxHeight = SystemParameters.WorkArea.Height - 32;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Content = new ScrollViewer { Content = fields, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };

        var tracks = new ComboBox
        {
            ItemsSource = savedTracks?.EnumerateArray()
                .Where(track => track.GetProperty("committed_chunks").GetInt64() > 0)
                .Select(track => new Track(Read(track, "id"),
                    Read(track, "source") == "mic" ? "Microphone" : "System loopback")).ToArray(),
            DisplayMemberPath = nameof(Track.Name),
            SelectedIndex = 0,
        };
        var providers = new ComboBox
        {
            ItemsSource = presets.EnumerateArray()
                .Select(provider => new Provider(Read(provider, "id"), Read(provider, "name"), Read(provider, "base_url"), Read(provider, "model_id")))
                .ToArray(),
            DisplayMemberPath = nameof(Provider.Name),
        };
        var baseUrl = new TextBox { MaxLength = 2048 };
        var model = new TextBox { MaxLength = 192 };
        var language = new ComboBox
        {
            ItemsSource = SpeechWorker.Languages.Skip(1).ToArray(),
            SelectedIndex = sourceLanguage > 0 ? sourceLanguage - 1 : -1,
        };
        var credential = new PasswordBox { MaxLength = 4096 };

        if (!summary) AddField("Saved track", tracks);
        AddField("Provider", providers);
        AddField("Base URL", baseUrl);
        AddField("Model ID", model);
        if (!summary) AddField("Audio language", language);
        var keyLabel = AddField("API key", credential);

        fields.Children.Add(new TextBlock
        {
            Text = summary
                ? "Saved transcript text will be sent to the service above, in parts for long sessions. Check its data handling and billing terms."
                : "Audio from the selected track will be sent to the service above. Check its data handling and billing terms.",
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 16, 0, 10),
        });
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
        var send = new Button { Content = summary ? "Send text and generate" : "Send audio for transcription", Padding = new Thickness(9, 5, 9, 5) };
        var cancel = new Button { Content = "Cancel", IsCancel = true, Padding = new Thickness(9, 5, 9, 5), Margin = new Thickness(8, 0, 0, 0) };
        buttons.Children.Add(send);
        buttons.Children.Add(cancel);
        fields.Children.Add(buttons);

        void Refresh()
        {
            using var password = credential.SecurePassword;
            send.IsEnabled = (summary || (tracks.SelectedItem is Track && language.SelectedIndex >= 0))
                && providers.SelectedItem is Provider provider
                && !string.IsNullOrWhiteSpace(model.Text)
                && !string.IsNullOrWhiteSpace(baseUrl.Text)
                && (provider.Id == "custom" || password.Length > 0);
        }

        providers.SelectionChanged += (_, _) =>
        {
            if (providers.SelectedItem is not Provider provider) return;
            baseUrl.IsReadOnly = provider.Id != "custom";
            keyLabel.Content = provider.Id == "custom" ? "API key (optional)" : "API key";
            baseUrl.Text = provider.BaseUrl;
            model.Text = provider.ModelId;
            credential.Clear();
            Refresh();
        };
        baseUrl.TextChanged += (_, _) =>
        {
            credential.Clear();
            Refresh();
        };
        model.TextChanged += (_, _) => Refresh();
        language.SelectionChanged += (_, _) => Refresh();
        tracks.SelectionChanged += (_, _) => Refresh();
        credential.PasswordChanged += (_, _) => Refresh();
        send.Click += (_, _) =>
        {
            if (!send.IsEnabled || providers.SelectedItem is not Provider provider) return;
            var settings = new
            {
                provider = provider.Id, base_url = provider.Id == "custom" ? baseUrl.Text.Trim() : null,
                model_id = model.Text.Trim(), api_key = credential.Password,
            };
            if (summary)
                Request = new { op = "summarize", session_id = sessionId, approved = true, settings };
            else if (tracks.SelectedItem is Track track)
                Request = new
                {
                    op = "transcribe_external", session_id = sessionId, track_id = track.Id,
                    language = SpeechWorker.LanguageCode(language.SelectedIndex + 1), approved = true, settings,
                };
            else return;
            credential.Clear();
            DialogResult = true;
        };
        Closed += (_, _) => credential.Clear();
        providers.SelectedIndex = 0;
        Refresh();
    }

    private Label AddField(string name, Control control)
    {
        var label = new Label { Content = name, Target = control, Padding = new Thickness(0, 6, 0, 3) };
        fields.Children.Add(label);
        fields.Children.Add(control);
        return label;
    }

    private static string Read(JsonElement value, string key) => value.GetProperty(key).GetString() ?? "";

    private sealed record Track(string Id, string Name);
    private sealed record Provider(string Id, string Name, string BaseUrl, string ModelId);
}
