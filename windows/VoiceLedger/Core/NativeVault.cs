using System;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace VoiceLedger.Core;

internal static class NativeVault
{
    private const int MaxPlaintext = 16 * 1024 * 1024;
    private const int EnvelopeOverhead = 52;

    internal static byte[] CreateKey() => Request(1, [], [], [], 128);

    internal static byte[] Seal(byte[] recovery, byte[] data)
    {
        ArgumentOutOfRangeException.ThrowIfGreaterThan(data.Length, MaxPlaintext);
        return Request(2, recovery, [], data, data.Length + EnvelopeOverhead);
    }

    internal static byte[] Open(byte[] recovery, byte[] expected, byte[] envelope)
    {
        ArgumentOutOfRangeException.ThrowIfGreaterThan(envelope.Length, MaxPlaintext + EnvelopeOverhead);
        return Request(3, recovery, expected, envelope, envelope.Length);
    }

    private static byte[] Request(int operation, byte[] recovery, byte[] expected, byte[] data, int capacity)
    {
        _ = NativeCore.AbiVersion;
        var output = new byte[capacity];
        try
        {
            int length = (int)NativeCore.RequireSuccess(Call(operation, recovery, (uint)recovery.Length,
                expected, (uint)expected.Length, data, (uint)data.Length, output, (uint)output.Length));
            return output.AsSpan(0, length).ToArray();
        }
        finally
        {
            CryptographicOperations.ZeroMemory(output);
        }
    }

    [DllImport("voiceledger_ffi", EntryPoint = "vl_vault_request_v1", ExactSpelling = true,
        CallingConvention = CallingConvention.Cdecl)]
    private static extern int Call(int operation, [In] byte[] recovery, uint recoveryLength,
        [In] byte[] expected, uint expectedLength, [In] byte[] data, uint length,
        [Out] byte[] output, uint capacity);
}
