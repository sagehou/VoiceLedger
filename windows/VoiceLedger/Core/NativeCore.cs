using System;
using System.Runtime.InteropServices;
using System.Text;

namespace VoiceLedger.Core;

internal enum SessionCommand
{
    Prepare = 1,
    Start = 2,
    Pause = 3,
    Resume = 4,
    Stop = 5,
    Finish = 6,
    Interrupt = 7
}

internal enum SessionState
{
    Idle = 0,
    Preparing = 1,
    Recording = 2,
    Paused = 3,
    Stopping = 4,
    Completed = 5,
    Interrupted = 6
}

internal enum Compatibility { Unknown = 0, Incompatible = 1, Compatible = 2 }
internal enum Recommendation { RecordingOnly = 0, CandidateForBenchmark = 1 }

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

    internal static int ValidateDomain(byte[] data) => (int)RequireSuccess(ValidateDomainNative(data, (uint)data.Length));

    internal static void SaveDeviceProfile(string directory, byte[] measurement)
    {
        byte[] path = Encoding.UTF8.GetBytes(directory);
        RequireSuccess(SaveDeviceProfileNative(path, (uint)path.Length, measurement, (uint)measurement.Length));
    }

    internal static ReadOnlyMemory<byte> ModelPreview(string directory)
    {
        byte[] path = Encoding.UTF8.GetBytes(directory);
        byte[] output = new byte[16 * 1024];
        int length = (int)RequireSuccess(ModelPreviewNative(path, (uint)path.Length, output, (uint)output.Length));
        return output.AsMemory(0, length);
    }

    internal static byte[] SpeechRequest(string request)
    {
        byte[] query = Encoding.UTF8.GetBytes(request);
        int result = SpeechRequestNative(query, (uint)query.Length, out AudioBuffer buffer);
        return ReadBuffer(result, buffer);
    }

    internal static byte[] SpeechPartial()
    {
        int result = SpeechPartialNative(out AudioBuffer buffer);
        return ReadBuffer(result, buffer);
    }

    internal static void UpdateSpeechResources(byte[] data) =>
        RequireSuccess(SpeechResourcesNative(data, (uint)data.Length));

    private static byte[] ReadBuffer(int result, AudioBuffer buffer)
    {
        try
        {
            RequireSuccess(result);
            if (buffer.Length == 0) return Array.Empty<byte>();
            byte[] output = new byte[checked((int)buffer.Length)];
            Marshal.Copy(buffer.Data, output, 0, output.Length);
            return output;
        }
        finally
        {
            ReleaseBuffer(buffer.Data, buffer.Length);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct AudioBuffer
    {
        internal IntPtr Data;
        internal uint Length;
    }

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

    [DllImport(Library, EntryPoint = "vl_domain_validate_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int ValidateDomainNative([In] byte[] data, uint length);

    [DllImport(Library, EntryPoint = "vl_device_profile_save_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int SaveDeviceProfileNative([In] byte[] path, uint pathLength,
        [In] byte[] measurement, uint measurementLength);

    [DllImport(Library, EntryPoint = "vl_model_preview_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int ModelPreviewNative([In] byte[] path, uint pathLength, [Out] byte[] output, uint capacity);

    [DllImport(Library, EntryPoint = "vl_speech_request_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int SpeechRequestNative([In] byte[] request, uint length, out AudioBuffer buffer);

    [DllImport(Library, EntryPoint = "vl_speech_partial_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int SpeechPartialNative(out AudioBuffer buffer);

    [DllImport(Library, EntryPoint = "vl_speech_resources_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int SpeechResourcesNative([In] byte[] data, uint length);

    [DllImport(Library, EntryPoint = "vl_audio_buffer_release_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern void ReleaseBuffer(IntPtr data, uint length);

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
