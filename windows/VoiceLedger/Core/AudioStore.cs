using System;
using System.Buffers.Binary;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace VoiceLedger.Core;

internal sealed record RecoveryReport(int RestoredChunks, int MissingChunks, int CorruptChunks, int UncommittedChunks,
    int QuarantinedFiles, int InterruptedSessions, long KnownTailLossNs, long ElapsedMs);

internal sealed record HistoryTrack(Guid Id, Guid SessionId, string Source, long CreatedAtMs, bool InterruptedTail,
    long FirstNs, long LastNs, int CommittedChunks, int LossIntervals)
{
    public string Display
    {
        get
        {
            string source = Source == "mic" ? "Microphone" : "System loopback";
            string evidence = InterruptedTail ? " · interrupted" : "";
            if (LossIntervals > 0) evidence += $" · {LossIntervals} gaps";
            return $"{DateTimeOffset.FromUnixTimeMilliseconds(CreatedAtMs).LocalDateTime:g} · {source} · {CommittedChunks} segments{evidence}";
        }
    }
}

internal sealed record TimelineRow(uint SourceEpoch, long StartNs, long EndNs, string Kind, string? Cause);
internal sealed record PlaybackChunk(int Channels, long NextNs, float[] Samples);

internal sealed class AudioStore : SafeHandle
{
    private const string Library = "voiceledger_ffi";
    internal const int PlaybackRate = 48_000;
    internal const int MaxChannels = 32;
    internal const int Unavailable = -22;
    internal const int Locked = -21;
    private static readonly JsonSerializerOptions Json = new() { PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower };

    internal AudioStore(string root) : base(IntPtr.Zero, ownsHandle: true)
    {
        _ = NativeCore.AbiVersion;
        Root = Path.GetFullPath(root);
        string volume = Path.GetPathRoot(Root) ?? throw new IOException("Recording volume is unavailable.");
        if (volume.StartsWith(@"\\", StringComparison.Ordinal) || new DriveInfo(volume).DriveType == DriveType.Network)
            throw new IOException("The recording library requires a local volume.");
        byte[] path = Encoding.UTF8.GetBytes(Root);
        SetHandle(new IntPtr(NativeCore.RequireSuccess(Open(path, (uint)path.Length))));
    }

    public override bool IsInvalid => handle == IntPtr.Zero;
    internal string Root { get; }
    internal string SessionDirectory(Guid sessionId) => Path.Combine(Root, "audio", sessionId.ToString("D"));

    internal void Write(object command, float[]? pcm = null) => _ = Request(command, pcm);
    internal RecoveryReport Recovery() => Read<RecoveryReport>(new { op = "recovery" });
    internal HistoryTrack[] History() => Read<HistoryTrack[]>(new { op = "history" });
    internal TimelineRow[] Timeline(Guid trackId) => Read<TimelineRow[]>(new { op = "timeline", trackId });

    internal PlaybackChunk Seek(Guid trackId, long positionNs)
    {
        byte[] result = Request(new { op = "seek", trackId, positionNs });
        const int header = 12;
        if (result.Length < header || (result.Length - header) % sizeof(float) != 0)
            throw new InvalidDataException("Invalid audio response.");
        int channels = BinaryPrimitives.ReadInt32LittleEndian(result);
        long next = BinaryPrimitives.ReadInt64LittleEndian(result.AsSpan(4));
        var samples = new float[(result.Length - header) / sizeof(float)];
        if (channels is < 1 or > MaxChannels || samples.Length % channels != 0 || next <= positionNs)
            throw new InvalidDataException("Invalid playback range.");
        Buffer.BlockCopy(result, header, samples, 0, result.Length - header);
        return new PlaybackChunk(channels, next, samples);
    }

    private T Read<T>(object command) where T : class => JsonSerializer.Deserialize<T>(Request(command), Json)
        ?? throw new InvalidDataException("Invalid audio response.");

    private byte[] Request(object command, float[]? pcm = null)
    {
        byte[] request = JsonSerializer.SerializeToUtf8Bytes(command, Json);
        int code = Execute(this, request, (uint)request.Length, pcm, (uint)(pcm?.Length ?? 0), out AudioBuffer result);
        try
        {
            NativeCore.RequireSuccess(code);
            if (result.Length == 0) return Array.Empty<byte>();
            var bytes = new byte[checked((int)result.Length)];
            Marshal.Copy(result.Data, bytes, 0, bytes.Length);
            return bytes;
        }
        finally
        {
            ReleaseBuffer(result.Data, result.Length);
        }
    }

    protected override bool ReleaseHandle() => Release(handle.ToInt64()) == 0;

    [StructLayout(LayoutKind.Sequential)]
    private struct AudioBuffer
    {
        internal IntPtr Data;
        internal uint Length;
    }

    [DllImport(Library, EntryPoint = "vl_audio_open_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern long Open([In] byte[] path, uint length);

    [DllImport(Library, EntryPoint = "vl_audio_request_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int Execute(AudioStore store, [In] byte[] command, uint length, [In] float[]? pcm, uint count,
        out AudioBuffer result);

    [DllImport(Library, EntryPoint = "vl_audio_buffer_release_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern void ReleaseBuffer(IntPtr data, uint length);

    [DllImport(Library, EntryPoint = "vl_audio_release_v1", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int Release(long store);
}
