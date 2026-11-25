using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using VoiceLedger.Core;

internal static class VaultSmoke
{
    internal static void Run(string fixture)
    {
        using var report = JsonDocument.Parse(File.ReadAllBytes(fixture));
        string Read(string field) => report.RootElement.GetProperty(field).GetString()!;
        byte[] expected = Convert.FromHexString(Read("object_id"));
        byte[] envelope = Convert.FromHexString(Read("envelope"));
        byte[] recovery = Encoding.ASCII.GetBytes(Read("recovery_code"));
        byte[] plaintext = Encoding.UTF8.GetBytes(Read("plaintext"));
        byte[] restored = VaultKeyProtection.Unwrap(VaultKeyProtection.Wrap(recovery));
        byte[] generated = NativeVault.CreateKey();
        byte[] rotated = NativeVault.Rotate(generated);
        string directory = Path.Combine(Path.GetTempPath(), $"voiceledger-vault-{Guid.NewGuid():N}");
        try
        {
            Expect(NativeVault.Open(restored, expected, envelope).AsSpan().SequenceEqual(plaintext));
            Expect(VaultKeyProtection.ReadLocal(directory) is null);
            VaultKeyProtection.InstallLocal(directory, generated);
            byte[] local = VaultKeyProtection.ReadLocal(directory) ?? throw new InvalidDataException("Wrapped key missing.");
            try
            {
                Expect(local.AsSpan().SequenceEqual(generated));
                Expect(!File.ReadAllBytes(Path.Combine(directory, "vault-key.v1")).AsSpan().SequenceEqual(generated));
                bool rejected = false;
                try { VaultKeyProtection.InstallLocal(directory, rotated); }
                catch (IOException) { rejected = true; }
                Expect(rejected);
            }
            finally { CryptographicOperations.ZeroMemory(local); }
            byte[] current = NativeVault.Identity(generated);
            byte[] next = NativeVault.Identity(rotated);
            Expect(current[..16].AsSpan().SequenceEqual(next[..16]));
            Expect(next[16..].AsSpan().SequenceEqual(new byte[] { 0, 0, 0, 2 }));
            byte[] fresh = NativeVault.Seal(generated, plaintext);
            byte[] id = fresh[8..24];
            Expect(NativeVault.Open(generated, id, fresh).AsSpan().SequenceEqual(plaintext));
            try
            {
                _ = NativeVault.Open(rotated, id, fresh);
                throw new InvalidDataException("Old vault data was accepted by a new generation.");
            }
            catch (CoreException error) when (error.Code == -50) { }
            fresh[^1] ^= 1;
            try
            {
                _ = NativeVault.Open(generated, id, fresh);
                throw new InvalidDataException("Tampered vault data was accepted.");
            }
            catch (CoreException error) when (error.Code == -50) { }
            Console.WriteLine("WPF passed vault recovery vectors, tamper rejection and current-user DPAPI.");
        }
        finally
        {
            CryptographicOperations.ZeroMemory(recovery);
            CryptographicOperations.ZeroMemory(restored);
            CryptographicOperations.ZeroMemory(generated);
            CryptographicOperations.ZeroMemory(rotated);
            if (Directory.Exists(directory)) Directory.Delete(directory, recursive: true);
        }
    }

    private static void Expect(bool valid)
    {
        if (!valid) throw new InvalidDataException("Vault fixture did not match.");
    }
}
