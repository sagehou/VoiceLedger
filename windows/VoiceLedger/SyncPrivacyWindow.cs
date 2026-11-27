using System;
using System.Buffers.Binary;
using System.IO;
using System.Linq;
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
    private readonly ComboBox devices = new() { Visibility = Visibility.Collapsed };
    private readonly StackPanel syncActions = new();
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
    private bool updatingKey;

    internal SyncPrivacyWindow()
    {
        Title = "Sync & privacy";
        Width = 460;
        Height = 580;
        MinWidth = 320;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Content = new ScrollViewer { Content = form, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
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
        AutomationProperties.SetName(devices, "Other vault devices");
        form.Children.Add(devices);
        form.Children.Add(syncActions);
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
        syncActions.Children.Clear();
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
                keyActions.Children.Add(Button("Show or hide recovery key", ShowKeyAsync));
                if (updatingKey)
                {
                    Field(keyActions, "Updated recovery key", import);
                    keyActions.Children.Add(Button("Join updated vault", JoinUpdatedAsync));
                }
                else
                {
                    keyActions.Children.Add(Button("Enter updated recovery key", () =>
                    {
                        updatingKey = true;
                        return RefreshKeyAsync();
                    }));
                }
                syncActions.Children.Add(Button("Sync text", SyncAsync));
                if (VaultKeyProtection.PendingTarget(KeyDirectory) is { } pending)
                    syncActions.Children.Add(Button("Resume device removal", () => RemoveAsync(pending)));
                else
                    syncActions.Children.Add(Button("Remove selected device", () => RemoveAsync(null)));
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

    private byte[] ProviderConfig()
    {
        object settings = provider.SelectedIndex == 0
            ? new { provider = "webdav", endpoint = endpoint.Text.Trim(), name = account.Text.Trim(), password = password.Password }
            : new
            {
                provider = "s3", endpoint = endpoint.Text.Trim(), bucket = bucket.Text.Trim(), region = region.Text.Trim(),
                access_key = account.Text.Trim(), secret_key = password.Password,
                session_token = token.Password.Length == 0 ? null : token.Password,
            };
        return JsonSerializer.SerializeToUtf8Bytes(settings);
    }

    private static object Command(object action) => new
    {
        recordings = Path.Combine(SpeechWorker.ProfileDirectory, "Recordings"),
        directory = KeyDirectory,
        action,
    };

    private Task SyncAsync() => RunTransferAsync("Syncing encrypted text…", (configuration, key) =>
        NativeSync.Transfer(Command(new { op = "synchronize" }), configuration, key), report =>
        {
            string? local = report.GetProperty("device").GetString();
            string[] other = report.GetProperty("devices").EnumerateArray()
                .Select(item => item.GetString()!)
                .Where(id => id != local).ToArray();
            devices.ItemsSource = other;
            devices.SelectedIndex = -1;
            devices.Visibility = other.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
            return $"Text synced · {report.GetProperty("imported").GetInt32()} revisions imported\nThis device · {local}";
        });

    private async Task JoinUpdatedAsync()
    {
        byte[] next = Encoding.ASCII.GetBytes(import.Password.Trim());
        import.Clear();
        try
        {
            await RunTransferAsync("Verifying updated recovery key…", (configuration, key) =>
            {
                var report = NativeSync.Transfer(Command(new { op = "verify_key" }), configuration, next);
                try
                {
                    VaultKeyProtection.CommitRotation(KeyDirectory, key, next);
                    return report;
                }
                catch { report.Dispose(); throw; }
            }, report => $"Vault generation {report.GetProperty("generation").GetUInt32()} joined.", refreshKey: true);
        }
        finally { CryptographicOperations.ZeroMemory(next); }
    }

    private async Task RemoveAsync(Guid? pending)
    {
        Guid target;
        if (pending is { } selected) target = selected;
        else if (!Guid.TryParse(devices.SelectedItem as string, out target))
        {
            providerStatus.Text = "Sync text and select the device to remove.";
            return;
        }
        if (pending is null && MessageBox.Show(this,
            "Removal creates a new vault key. Share it with trusted devices. Previously copied data remains on those devices.",
            "Remove device", MessageBoxButton.OKCancel, MessageBoxImage.Warning) != MessageBoxResult.OK)
            return;
        await RunTransferAsync("Removing device…", (configuration, key) =>
        {
            byte[] next = VaultKeyProtection.PrepareRotation(KeyDirectory, key, target);
            try
            {
                var report = NativeSync.Transfer(Command(new { op = "remove_device", target }), configuration, key, next);
                try
                {
                    VaultKeyProtection.CommitRotation(KeyDirectory, key, next);
                    return report;
                }
                catch { report.Dispose(); throw; }
            }
            finally { CryptographicOperations.ZeroMemory(next); }
        }, report => $"Device removed · Share the generation {report.GetProperty("generation").GetUInt32()} recovery key.", refreshKey: true);
    }

    private async Task RunTransferAsync(string progress, Func<byte[], byte[], JsonDocument> run,
        Func<JsonElement, string> success, bool refreshKey = false)
    {
        byte[] configuration = ProviderConfig();
        providerStatus.Text = progress;
        try
        {
            using var report = await Task.Run(() => UseKey(key => run(configuration, key)));
            if (closed) return;
            if (report is null)
            {
                providerStatus.Text = "Set up the vault key first.";
                return;
            }
            providerStatus.Text = success(report.RootElement);
            if (refreshKey)
            {
                updatingKey = false;
                shownKey.Clear();
                shownKey.Visibility = Visibility.Collapsed;
                devices.ItemsSource = null;
                devices.Visibility = Visibility.Collapsed;
                await RefreshKeyAsync();
            }
        }
        catch (CoreException failure)
        {
            if (!closed) providerStatus.Text = ProviderError(failure.Code);
            if (refreshKey && !closed) await RefreshKeyAsync();
        }
        catch (Exception failure) when (KeyError(failure) || failure is JsonException)
        {
            if (!closed) providerStatus.Text = "The vault operation could not finish. Check the recovery key or retry.";
        }
        finally { CryptographicOperations.ZeroMemory(configuration); }
    }

    private async Task CheckAsync()
    {
        byte[] request = ProviderConfig();
        providerStatus.Text = "Checking provider…";
        try
        {
            await Task.Run(() => NativeSync.CheckProvider(request));
            if (!closed) providerStatus.Text = "Provider check passed.";
        }
        catch (CoreException failure)
        {
            if (!closed) providerStatus.Text = ProviderError(failure.Code);
        }
        catch (Exception failure) when (failure is TypeInitializationException or EntryPointNotFoundException)
        {
            if (!closed) providerStatus.Text = "The provider check is unavailable.";
        }
        finally { CryptographicOperations.ZeroMemory(request); }
    }

    private static string ProviderError(int code) => code switch
    {
        -60 => "Check the endpoint and required account fields.",
        -61 => "The provider rejected the credentials.",
        -62 => "The provider is out of storage.",
        -63 => "The provider did not support the required storage operations.",
        -65 => "The stored vault data could not be verified.",
        -66 => "This vault generation has retired. Join with the current recovery key.",
        -67 => "This device was removed from the vault.",
        _ => "The provider could not be reached. Check the connection and retry.",
    };

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
