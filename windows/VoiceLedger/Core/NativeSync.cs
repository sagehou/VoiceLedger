using System.Runtime.InteropServices;

namespace VoiceLedger.Core;

internal static class NativeSync
{
    internal static void CheckProvider(byte[] configuration)
    {
        _ = NativeCore.AbiVersion;
        NativeCore.RequireSuccess(Check(configuration, (uint)configuration.Length));
    }

    [DllImport("voiceledger_ffi", EntryPoint = "vl_sync_check_provider_v1",
        ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int Check([In] byte[] configuration, uint length);
}
