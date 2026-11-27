using System;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace VoiceLedger.Core;

internal static class NativeSync
{
    internal static void CheckProvider(byte[] configuration)
    {
        _ = NativeCore.AbiVersion;
        NativeCore.RequireSuccess(Check(configuration, (uint)configuration.Length));
    }

    internal static JsonDocument Transfer(object command, byte[] configuration, byte[] recovery, byte[]? successor = null)
    {
        _ = NativeCore.AbiVersion;
        byte[] request = JsonSerializer.SerializeToUtf8Bytes(command);
        byte[] replacement = successor ?? [];
        int code = Run(request, (uint)request.Length, configuration, (uint)configuration.Length,
            recovery, (uint)recovery.Length, replacement, (uint)replacement.Length, out var buffer);
        try
        {
            NativeCore.RequireSuccess(code);
            var bytes = new byte[checked((int)buffer.Length)];
            Marshal.Copy(buffer.Data, bytes, 0, bytes.Length);
            return JsonDocument.Parse(bytes);
        }
        finally { Release(buffer.Data, buffer.Length); }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeBuffer
    {
        internal IntPtr Data;
        internal uint Length;
    }

    [DllImport("voiceledger_ffi", EntryPoint = "vl_sync_request_v1",
        ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int Run([In] byte[] command, uint commandLength, [In] byte[] provider, uint providerLength,
        [In] byte[] recovery, uint recoveryLength, [In] byte[] successor, uint successorLength, out NativeBuffer output);

    [DllImport("voiceledger_ffi", EntryPoint = "vl_audio_buffer_release_v1",
        ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern void Release(IntPtr buffer, uint length);

    [DllImport("voiceledger_ffi", EntryPoint = "vl_sync_check_provider_v1",
        ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int Check([In] byte[] configuration, uint length);
}
