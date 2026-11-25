using System;
using System.Buffers.Binary;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using VoiceLedger.Core;

namespace VoiceLedger;

internal sealed class SyncPrivacyWindow : Window
{
    private static readonly string KeyDirectory = Path.Combine(SpeechWorker.ProfileDirectory, "Sync");
    private readonly StackPanel form = new() { Margin = new Thickness(20) };
    private readonly StackPanel keyActions = new();
    private readonly TextBlock keyStatus = Status();
    private readonly TextBlock providerStatus = Status();
    private readonly TextBox shownKey = new() { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly PasswordBox import = new() { MaxLength = 128 };
    private readonly PasswordBox password = new() { MaxLength = 4096 };
    private readonly PasswordBox token = new() { MaxLength = 16384 };
    private readonly TextBox endpoint = new() { MaxLength = 2048 };
    private readonly TextBox account = new() { MaxLength = 128 };
    private readonly TextBox bucket = new() { MaxLength = 63 };
    private readonly TextBox region = new() { MaxLength = 64 };
    private readonly ComboBox provider = new() { ItemsSource = new[] { "WebDAV", "S3-compatible" }, SelectedIndex = 0 };
    private bool closed;
    private bool settingUp;

    internal SyncPrivacyWindow()
    {
        Title = "Sync & privacy";
        Width = 460;
        Height = 580;
        MinWidth = 320;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Content = new ScrollViewer { Content = form, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        form.Children.Add(new TextBlock { Text = "Sync disabled", FontWeight = FontWeights.SemiBold });
        form.Children.Add(keyStatus);
        form.Children.Add(keyActions);
        AutomationProperties.SetName(shownKey, "Recovery key");
        form.Children.Add(shownKey);
        Field(form, "Provider", provider);
        Field(form, "HTTPS endpoint", endpoint);
        var accountLabel = Field(form, "Account", account);
        var passwordLabel = Field(form, "Password", password);
        var s3 = new StackPanel { Visibility = Visibility.Collapsed };
        Field(s3, "Bucket", bucket);
        Field(s3, "Region", region);
        Field(s3, "Session token (optional)", token);
        form.Children.Add(s3);
        provider.SelectionChanged += (_, _) =>
        {
            bool objectStorage = provider.SelectedIndex == 1;
            s3.Visibility = objectStorage ? Visibility.Visible : Visibility.Collapsed;
            accountLabel.Content = objectStorage ? "Access key" : "Account";
            passwordLabel.Content = objectStorage ? "Secret key" : "Password";
            password.Clear();
            token.Clear();
        };
        var check = Button("Check provider", CheckAsync);
        check.ToolTip = "Tests an encrypted synthetic object in a separate random folder.";
        form.Children.Add(check);
        form.Children.Add(providerStatus);
        Loaded += async (_, _) => await RefreshKeyAsync();
    }

    protected override void OnClosed(EventArgs e)
    {
        closed = true;
        shownKey.Clear();
        import.Clear();
        password.Clear();
        token.Clear();
        base.OnClosed(e);
    }

    private async Task RefreshKeyAsync()
    {
        keyActions.Children.Clear();
        keyStatus.Text = "Checking local vault key…";
        try
        {
            byte[]? identity = await Task.Run(() => UseKey(NativeVault.Identity));
            if (closed) return;
            if (identity is null)
            {
                keyStatus.Text = "No vault key on this device.";
                if (settingUp)
                {
                    keyActions.Children.Add(Button("Create vault key", () => InstallAsync(create: true)));
                    Field(keyActions, "Recovery key", import);
                    keyActions.Children.Add(Button("Import recovery key", () => InstallAsync(create: false)));
                }
                else
                {
                    keyActions.Children.Add(Button("Set up vault", () =>
                    {
                        settingUp = true;
                        return RefreshKeyAsync();
                    }));
                }
            }
            else
            {
                uint generation = BinaryPrimitives.ReadUInt32BigEndian(identity.AsSpan(16));
                keyStatus.Text = $"Vault key ready · Generation {generation}";
                keyActions.Children.Add(Button("Show recovery key", ShowKeyAsync));
            }
        }
        catch (Exception failure) when (KeyError(failure))
        {
            if (!closed) keyStatus.Text = "The local vault key is unavailable.";
        }
    }

    private async Task InstallAsync(bool create)
    {
        byte[] submitted = create ? [] : Encoding.ASCII.GetBytes(import.Password.Trim());
        import.Clear();
        try
        {
            await Task.Run(() =>
            {
                byte[] recovery = create ? NativeVault.CreateKey() : submitted;
                try { VaultKeyProtection.InstallLocal(KeyDirectory, recovery); }
                finally { CryptographicOperations.ZeroMemory(recovery); }
            });
            if (!closed) await RefreshKeyAsync();
        }
        catch (Exception failure) when (KeyError(failure))
        {
            if (!closed) keyStatus.Text = "The vault key could not be saved. Check the recovery key or reopen this screen.";
        }
        finally { CryptographicOperations.ZeroMemory(submitted); }
    }

    private async Task ShowKeyAsync()
    {
        if (shownKey.Visibility == Visibility.Visible)
        {
            shownKey.Clear();
            shownKey.Visibility = Visibility.Collapsed;
            return;
        }
        try
        {
            string? key = await Task.Run(() => UseKey<string>(Encoding.ASCII.GetString));
            if (closed || key is null) return;
            shownKey.Text = key;
            shownKey.Visibility = Visibility.Visible;
        }
        catch (Exception failure) when (KeyError(failure))
        {
            if (!closed) keyStatus.Text = "The local vault key is unavailable.";
        }
    }

    private async Task CheckAsync()
    {
        object settings = provider.SelectedIndex == 0
            ? new { provider = "webdav", endpoint = endpoint.Text.Trim(), name = account.Text.Trim(), password = password.Password }
            : new
            {
                provider = "s3", endpoint = endpoint.Text.Trim(), bucket = bucket.Text.Trim(), region = region.Text.Trim(),
                access_key = account.Text.Trim(), secret_key = password.Password,
                session_token = token.Password.Length == 0 ? null : token.Password,
            };
        byte[] request = JsonSerializer.SerializeToUtf8Bytes(settings);
        providerStatus.Text = "Checking provider…";
        try
        {
            await Task.Run(() => NativeSync.CheckProvider(request));
            if (!closed) providerStatus.Text = "Provider check passed.";
        }
        catch (CoreException failure)
        {
            if (!closed) providerStatus.Text = failure.Code switch
            {
                -60 => "Check the endpoint and required account fields.",
                -61 => "The provider rejected the credentials.",
                -62 => "The provider is out of storage.",
                -63 => "The provider did not support the required storage operations.",
                -65 => "The stored test object could not be verified.",
                _ => "The provider could not be reached. Check the connection and retry.",
            };
        }
        catch (Exception failure) when (failure is TypeInitializationException or EntryPointNotFoundException)
        {
            if (!closed) providerStatus.Text = "The provider check is unavailable.";
        }
        finally { CryptographicOperations.ZeroMemory(request); }
    }

    private static T? UseKey<T>(Func<byte[], T> action) where T : class
    {
        byte[]? key = VaultKeyProtection.ReadLocal(KeyDirectory);
        if (key is null) return null;
        try { return action(key); }
        finally { CryptographicOperations.ZeroMemory(key); }
    }

    private static bool KeyError(Exception failure) => failure is IOException or UnauthorizedAccessException
        or InvalidOperationException or System.ComponentModel.Win32Exception or TypeInitializationException
        or EntryPointNotFoundException;

    private static TextBlock Status()
    {
        var text = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 4, 0, 6) };
        AutomationProperties.SetLiveSetting(text, AutomationLiveSetting.Polite);
        return text;
    }

    private static Label Field(Panel parent, string name, Control field)
    {
        var label = new Label { Content = name, Target = field };
        parent.Children.Add(label);
        parent.Children.Add(field);
        return label;
    }

    private Button Button(string text, Func<Task> action)
    {
        var button = new Button { Content = text, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 5, 0, 0) };
        button.Click += async (_, _) =>
        {
            form.IsEnabled = false;
            try { await action(); }
            finally { form.IsEnabled = true; }
        };
        return button;
    }
}
