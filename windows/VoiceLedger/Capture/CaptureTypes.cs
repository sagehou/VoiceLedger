using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
using NAudio.CoreAudioApi;
using NAudio.Wave;

namespace VoiceLedger.Capture;

internal enum CaptureSource { Mic, System }
internal enum SourceMode { Mic, System, Both }

internal enum CaptureEnd
{
    Stop, Pause, EndpointLost, FormatChanged, SystemSuspend, QueueOverflow, StorageFailure, SourceFailure, UnsupportedFormat
}

internal sealed record CaptureEndpoint(string Id, string Name, CaptureSource Source, DeviceState State, bool IsDefault, string MixFormat)
{
    internal bool IsAvailable => State == DeviceState.Active;
    public string Display => $"{Name}{(IsDefault ? " (default)" : "")} · {State} · {MixFormat}";
}

internal sealed class CaptureClock
{
    private readonly long origin = Stopwatch.GetTimestamp();
    internal long OriginQpc100ns => (long)((Int128)origin * 10_000_000 / Stopwatch.Frequency);
    internal long NowNs => (long)((Int128)(Stopwatch.GetTimestamp() - origin) * 1_000_000_000 / Stopwatch.Frequency);

    internal long? MapQpc(long qpc100ns, long observedAtNs)
    {
        long start = OriginQpc100ns;
        if (qpc100ns < start || (Int128)(qpc100ns - start) * 100 > observedAtNs)
        {
            return null;
        }
        return (qpc100ns - start) * 100;
    }

    internal static long FrameDuration(int frames, int sampleRate) => (long)frames * 1_000_000_000 / sampleRate;
}

internal readonly record struct PacketStamp(int Frames, long Position, long Qpc100ns, AudioClientBufferFlags Flags);
internal readonly record struct CapturedPacket(PacketStamp Stamp, long ObservedAtNs, byte[]? Buffer, int Length);
internal readonly record struct CaptureInterval(string Kind, long StartNs, long EndNs, string? Cause = null);
internal readonly record struct PlacedPacket(long StartNs, long EndNs, bool Estimated, CaptureInterval? Before);
internal sealed record CaptureStop(CaptureEnd Reason, long AtNs)
{
    internal bool Graceful => Reason is CaptureEnd.Stop or CaptureEnd.Pause;
}

internal sealed record EpochStart(long AtNs, WaveFormat Format, int BufferFrames);
internal sealed record EpochResult(CaptureEnd Reason, long EndNs, long CoveredUntilNs, bool Started);

internal delegate void PacketConsumer(nint data, PacketStamp stamp);
internal delegate ICaptureInput OpenCaptureInput(CaptureEndpoint endpoint, Action<CaptureEnd> interrupted);

internal interface ICaptureInput : IDisposable
{
    WaveFormat Format { get; }
    int BufferFrames { get; }
    WaitHandle PacketReady { get; }
    void Start();
    void Stop();
    bool TryRead(PacketConsumer consume);
}

internal static class CaptureFormat
{
    internal static string Signature(WaveFormat format)
    {
        using var stream = new MemoryStream();
        using var writer = new BinaryWriter(stream);
        format.Serialize(writer);
        return Convert.ToBase64String(stream.ToArray());
    }
}

internal static class CaptureReason
{
    internal static string Code(CaptureEnd reason) => reason switch
    {
        CaptureEnd.Stop => "user_stop",
        CaptureEnd.Pause => "paused",
        CaptureEnd.EndpointLost => "endpoint_lost",
        CaptureEnd.FormatChanged => "format_changed",
        CaptureEnd.SystemSuspend => "system_suspend",
        CaptureEnd.QueueOverflow => "queue_overflow",
        CaptureEnd.StorageFailure => "writer_failure",
        _ => "unknown_capture"
    };
}
