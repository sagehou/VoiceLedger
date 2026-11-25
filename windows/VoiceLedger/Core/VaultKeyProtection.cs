using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace VoiceLedger.Core;

internal static class VaultKeyProtection
{
    private const int UiForbidden = 1;

    internal static byte[] Wrap(byte[] recovery) => WithDpapi(recovery, protecting: true);
    internal static byte[] Unwrap(byte[] wrapped) => WithDpapi(wrapped, protecting: false);

    internal static byte[]? ReadLocal(string directory)
    {
        byte[] wrapped;
        try
        {
            using var file = File.OpenRead(Path.Combine(directory, "vault-key.v1"));
            if (file.Length is < 1 or > 4096) throw new InvalidDataException("Invalid wrapped key length.");
            wrapped = new byte[(int)file.Length];
            file.ReadExactly(wrapped);
        }
        catch (Exception failure) when (failure is FileNotFoundException or DirectoryNotFoundException)
        {
            return null;
        }
        byte[] recovery = Unwrap(wrapped);
        try
        {
            _ = NativeVault.Identity(recovery);
            return recovery;
        }
        catch
        {
            CryptographicOperations.ZeroMemory(recovery);
            throw;
        }
        finally { CryptographicOperations.ZeroMemory(wrapped); }
    }

    internal static void InstallLocal(string directory, byte[] recovery)
    {
        _ = NativeVault.Identity(recovery);
        Directory.CreateDirectory(directory);
        byte[] wrapped = Wrap(recovery);
        string temporary = Path.Combine(directory, $".vault-key-{Guid.NewGuid():N}.partial");
        try
        {
            using (var file = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                file.Write(wrapped);
                file.Flush(flushToDisk: true);
            }
            File.Move(temporary, Path.Combine(directory, "vault-key.v1"));
        }
        finally
        {
            CryptographicOperations.ZeroMemory(wrapped);
            File.Delete(temporary);
        }
    }

    private static byte[] WithDpapi(byte[] bytes, bool protecting)
    {
        if (bytes.Length is < 1 or > 4096) throw new ArgumentException("Invalid vault key length.");
        var input = new DataBlob { Size = bytes.Length, Pointer = Marshal.AllocHGlobal(bytes.Length) };
        DataBlob output = default;
        try
        {
            Marshal.Copy(bytes, 0, input.Pointer, bytes.Length);
            bool success = protecting
                ? CryptProtectData(ref input, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, UiForbidden, out output)
                : CryptUnprotectData(ref input, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, UiForbidden, out output);
            if (!success) throw new Win32Exception(Marshal.GetLastWin32Error());
            if (output.Size is < 1 or > 4096) throw new InvalidOperationException("Invalid wrapped key length.");
            var result = new byte[output.Size];
            Marshal.Copy(output.Pointer, result, 0, result.Length);
            return result;
        }
        finally
        {
            Clear(input);
            Marshal.FreeHGlobal(input.Pointer);
            if (output.Pointer != IntPtr.Zero)
            {
                Clear(output);
                _ = LocalFree(output.Pointer);
            }
        }
    }

    private static void Clear(DataBlob blob)
    {
        for (int index = 0; index < blob.Size; index++)
            Marshal.WriteByte(blob.Pointer, index, 0);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DataBlob
    {
        internal int Size;
        internal IntPtr Pointer;
    }

    [DllImport("crypt32", ExactSpelling = true, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptProtectData(ref DataBlob input, IntPtr description, IntPtr entropy,
        IntPtr reserved, IntPtr prompt, int flags, out DataBlob output);

    [DllImport("crypt32", ExactSpelling = true, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(ref DataBlob input, IntPtr description, IntPtr entropy,
        IntPtr reserved, IntPtr prompt, int flags, out DataBlob output);

    [DllImport("kernel32", ExactSpelling = true)]
    private static extern IntPtr LocalFree(IntPtr memory);
}
