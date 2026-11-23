using System;
using System.Runtime.InteropServices;

namespace VoiceLedger.Core;

internal enum SessionCommand { Prepare = 1, Start, Pause, Resume, Stop, Finish, Interrupt }
internal enum SessionState { Idle, Preparing, Recording, Paused, Stopping, Completed, Interrupted }
internal enum Compatibility { Unknown, Incompatible, Compatible }
internal enum Recommendation { RecordingOnly, CandidateForBenchmark }

internal sealed class CoreException(int code) : InvalidOperationException($"Shared core error {code}.")
{
    internal int Code { get; } = code;
}

internal sealed class CoreSession : SafeHandle
{
    internal CoreSession() : base(IntPtr.Zero, ownsHandle: true)
    {
        SetHandle(new IntPtr(NativeCore.RequireSuccess(NativeCore.Create())));
    }

    public override bool IsInvalid => handle == IntPtr.Zero;

    internal SessionState Apply(SessionCommand command, long atNs) =>
        (SessionState)NativeCore.RequireSuccess(NativeCore.Transition(this, (int)command, atNs));

    internal void Cancel() => NativeCore.RequireSuccess(NativeCore.Cancel(this));

    protected override bool ReleaseHandle() => NativeCore.Release(handle.ToInt64()) == 0;
}

internal static class NativeCore
{
    private const string Library = "voiceledger_ffi";

    static NativeCore()
    {
        AbiVersion = GetAbiVersion();
        if (AbiVersion != 1)
        {
            throw new InvalidOperationException($"Unsupported shared core ABI {AbiVersion}.");
        }
    }

    internal static int AbiVersion { get; }

    internal static Recommendation Recommend(Compatibility compatibility, long? budgetBytes, long? peakBytes) =>
        (Recommendation)RequireSuccess(RecommendNative((int)compatibility, budgetBytes ?? -1, peakBytes ?? -1));

    internal static long RequireSuccess(long result)
    {
        if (result < 0)
        {
            throw new CoreException((int)result);
        }
        return result;
    }

    [DllImport(Library, EntryPoint = "vl_abi_version_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int GetAbiVersion();

    [DllImport(Library, EntryPoint = "vl_session_create_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    internal static extern long Create();

    [DllImport(Library, EntryPoint = "vl_session_transition_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int Transition(CoreSession session, int command, long atNs);

    [DllImport(Library, EntryPoint = "vl_session_cancel_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int Cancel(CoreSession session);

    [DllImport(Library, EntryPoint = "vl_session_release_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int Release(long handle);

    [DllImport(Library, EntryPoint = "vl_recommend_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int RecommendNative(int compatibility, long budgetBytes, long peakBytes);
}
