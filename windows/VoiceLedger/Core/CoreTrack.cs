using System;
using System.Runtime.InteropServices;
using System.Text;

namespace VoiceLedger.Core;

internal enum TrackCommand { Start = 1, Pause = 2, Resume = 3, Disconnect = 4, Reconnect = 5, Stop = 6, Finish = 7, Interrupt = 8 }
internal enum TrackState { Preparing = 0, Recording = 1, Paused = 2, Disconnected = 3, Stopping = 4, Completed = 5, Interrupted = 6 }

internal sealed class CoreTrack : SafeHandle
{
    private const string Library = "voiceledger_ffi";

    internal CoreTrack(string endpoint) : base(IntPtr.Zero, ownsHandle: true)
    {
        _ = NativeCore.AbiVersion;
        byte[] id = Encoding.UTF8.GetBytes(endpoint);
        SetHandle(new IntPtr(NativeCore.RequireSuccess(CreateNative(id, (uint)id.Length))));
    }

    public override bool IsInvalid => handle == IntPtr.Zero;
    internal TrackState State { get; private set; } = TrackState.Preparing;
    internal uint Epoch => (uint)NativeCore.RequireSuccess(GetEpoch(this));

    internal void Apply(TrackCommand command, long atNs, string? confirmedEndpoint = null)
    {
        byte[]? id = confirmedEndpoint is null ? null : Encoding.UTF8.GetBytes(confirmedEndpoint);
        State = (TrackState)NativeCore.RequireSuccess(Transition(this, (int)command, atNs, id, (uint)(id?.Length ?? 0)));
    }

    protected override bool ReleaseHandle() => ReleaseNative(handle.ToInt64()) == 0;

    [DllImport(Library, EntryPoint = "vl_track_create_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern long CreateNative([In] byte[] endpoint, uint length);

    [DllImport(Library, EntryPoint = "vl_track_transition_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int Transition(CoreTrack track, int command, long atNs, [In] byte[]? endpoint, uint length);

    [DllImport(Library, EntryPoint = "vl_track_source_epoch_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern long GetEpoch(CoreTrack track);

    [DllImport(Library, EntryPoint = "vl_track_release_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int ReleaseNative(long track);
}
