using System;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace VoiceLedger.Core;

internal static class VaultKeyProtection
{
    private const int UiForbidden = 1;
    private const string ActiveKey = "vault-key.v1";
    private const string PendingPrefix = "vault-rotation-";

    internal static byte[] Wrap(byte[] recovery) => WithDpapi(recovery, protecting: true);
    internal static byte[] Unwrap(byte[] wrapped) => WithDpapi(wrapped, protecting: false);

    internal static byte[]? ReadLocal(string directory) => Read(directory, ActiveKey);

    internal static Guid? PendingTarget(string directory)
    {
        if (!Directory.Exists(directory)) return null;
        string? name = Directory.EnumerateFiles(directory, $"{PendingPrefix}*.v1").SingleOrDefault();
        if (name is null) return null;
        if (!Guid.TryParseExact(Path.GetFileNameWithoutExtension(name)[PendingPrefix.Length..], "D", out Guid target))
            throw new InvalidDataException("Invalid pending device removal.");
        return target;
    }

    internal static byte[] PrepareRotation(string directory, byte[] current, Guid target)
    {
        using var access = Lock(directory);
        RequireCurrent(directory, current);
        if (PendingTarget(directory) is { } existing && existing != target)
            throw new InvalidOperationException("A device removal is already in progress.");
        string pendingName = $"{PendingPrefix}{target:D}.v1";
        byte[]? pending = Read(directory, pendingName);
        if (pending is not null)
        {
            if (!pending.AsSpan().SequenceEqual(current)) return pending;
            CryptographicOperations.ZeroMemory(pending);
            File.Delete(Path.Combine(directory, pendingName));
        }
        byte[] next = NativeVault.Rotate(current);
        try
        {
            Install(directory, pendingName, next, overwrite: false);
            return next;
        }
        catch
        {
            CryptographicOperations.ZeroMemory(next);
            throw;
        }
    }

    internal static void CommitRotation(string directory, byte[] current, byte[] next)
    {
        using var access = Lock(directory);
        RequireCurrent(directory, current);
        byte[] prior = NativeVault.Identity(current);
        byte[] successor = NativeVault.Identity(next);
        if (!prior.AsSpan(0, 16).SequenceEqual(successor.AsSpan(0, 16))
            || System.Buffers.Binary.BinaryPrimitives.ReadUInt32BigEndian(prior.AsSpan(16))
                >= System.Buffers.Binary.BinaryPrimitives.ReadUInt32BigEndian(successor.AsSpan(16)))
            throw new InvalidOperationException("The recovery key is not a newer generation of this vault.");
        Install(directory, ActiveKey, next, overwrite: true);
        if (PendingTarget(directory) is { } target)
            File.Delete(Path.Combine(directory, $"{PendingPrefix}{target:D}.v1"));
    }

    private static byte[]? Read(string directory, string name)
    {
        byte[] wrapped;
        try
        {
            using var file = File.OpenRead(Path.Combine(directory, name));
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
        using var access = Lock(directory);
        Install(directory, ActiveKey, recovery, overwrite: false);
    }

    private static FileStream Lock(string directory)
    {
        Directory.CreateDirectory(directory);
        return new FileStream(Path.Combine(directory, "vault.lock"), FileMode.OpenOrCreate,
            FileAccess.ReadWrite, FileShare.None);
    }

    private static void RequireCurrent(string directory, byte[] expected)
    {
        byte[] saved = ReadLocal(directory) ?? throw new InvalidOperationException("Local vault key is unavailable.");
        try
        {
            if (!CryptographicOperations.FixedTimeEquals(expected, saved))
                throw new InvalidOperationException("Local vault key changed.");
        }
        finally { CryptographicOperations.ZeroMemory(saved); }
    }

    private static void Install(string directory, string name, byte[] recovery, bool overwrite)
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
            File.Move(temporary, Path.Combine(directory, name), overwrite);
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
