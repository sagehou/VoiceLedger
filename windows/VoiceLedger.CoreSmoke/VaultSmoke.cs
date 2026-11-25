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
        try
        {
            Expect(NativeVault.Open(restored, expected, envelope).AsSpan().SequenceEqual(plaintext));
            byte[] fresh = NativeVault.Seal(generated, plaintext);
            byte[] id = fresh[8..24];
            Expect(NativeVault.Open(generated, id, fresh).AsSpan().SequenceEqual(plaintext));
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
        }
    }

    private static void Expect(bool valid)
    {
        if (!valid) throw new InvalidDataException("Vault fixture did not match.");
    }
}
