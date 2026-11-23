using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using NAudio.Wave;
using VoiceLedger.Core;

namespace VoiceLedger.Capture;

internal readonly record struct PendingGap(uint Epoch, long StartNs, string Cause);

internal sealed class StorageReserveException() : IOException("The recording volume is below the 64 MiB reserve.") { }

internal sealed class TrackFiles(AudioStore audioStore, CaptureEndpoint endpoint)
{
    private const long CheckpointNs = 250_000_000;
    private readonly AudioStore store = audioStore;
    private long savedThroughNs;
    private long accountedThroughNs;
    private int sealedSegments;
    private int lossIntervals;

    internal Guid Id { get; } = Guid.NewGuid();
    internal CaptureEndpoint Endpoint { get; } = endpoint;
    internal int SealedSegments => Volatile.Read(ref sealedSegments);
    internal int LossIntervals => Volatile.Read(ref lossIntervals);
    internal long SavedThroughNs => Interlocked.Read(ref savedThroughNs);
    internal long AccountedThroughNs => Interlocked.Read(ref accountedThroughNs);

    internal void BeginEpoch(uint epoch, EpochStart start, PendingGap? prefix)
    {
        if (prefix is PendingGap gap)
            AddInterval(gap.Epoch, new CaptureInterval("gap", gap.StartNs, start.AtNs, gap.Cause));
        store.Write(new { op = "begin_stream", trackId = Id, epoch, startNs = start.AtNs,
            rate = start.Format.SampleRate, channels = start.Format.Channels,
            sampleFormat = ChunkPcm.SampleFormat(start.Format), nativeFormat = CaptureFormat.Signature(start.Format) });
        Interlocked.Exchange(ref accountedThroughNs, start.AtNs);
    }

    internal void AddInterval(uint epoch, CaptureInterval interval)
    {
        if (interval.EndNs <= interval.StartNs) return;
        store.Write(new { op = "interval", trackId = Id, epoch, interval.StartNs, interval.EndNs, interval.Kind, interval.Cause });
        Interlocked.Exchange(ref accountedThroughNs, interval.EndNs);
        if (interval.Kind == "gap" && interval.Cause != "paused")
            Interlocked.Increment(ref lossIntervals);
    }

    internal void EndEpoch(uint epoch, EpochResult result) => store.Write(new
    {
        op = "end_stream", trackId = Id, epoch, result.EndNs, reason = CaptureReason.Code(result.Reason)
    });

    internal Segment StartSegment(uint epoch, WaveFormat format, PlacedPacket first)
    {
        CheckReserve(store.Root);
        var id = Guid.NewGuid();
        store.Write(new { op = "begin_chunk", id, trackId = Id, epoch, first.StartNs, first.EndNs });
        Interlocked.Exchange(ref accountedThroughNs, first.EndNs);
        return new Segment(this, id, format, first.StartNs);
    }

    internal static void CheckReserve(string path)
    {
        string root = Path.GetPathRoot(Path.GetFullPath(path)) ?? throw new IOException("Recording volume is unavailable.");
        const long reserve = 64L * 1024 * 1024;
        if (new DriveInfo(root).AvailableFreeSpace < reserve)
            throw new StorageReserveException();
    }

    internal sealed class Segment(TrackFiles owner, Guid id, WaveFormat format, long start) : IDisposable
    {
        private const long TargetNs = 2_000_000_000;
        private const int MaxBytes = 8 * 1024 * 1024;
        private readonly MemoryStream audio = new();
        private readonly List<PacketIndex> packets = new();

        internal long EndNs { get; private set; } = start;

        internal bool CanAdd(CapturedPacket packet, PlacedPacket place) =>
            place.StartNs == EndNs && place.EndNs - start <= TargetNs
            && audio.Length + packet.Length <= MaxBytes && packets.Count < 512;

        internal void Write(CapturedPacket packet, PlacedPacket place)
        {
            audio.Write(packet.Buffer!, 0, packet.Length);
            packets.Add(new PacketIndex(place.StartNs, place.EndNs, packet.Stamp.Frames, packet.Stamp.Position,
                packet.Stamp.Qpc100ns, (int)packet.Stamp.Flags, place.Estimated));
            EndNs = place.EndNs;
            if (EndNs - owner.AccountedThroughNs >= CheckpointNs)
                Checkpoint();
        }

        internal void Seal()
        {
            Checkpoint();
            CheckReserve(owner.store.Root);
            float[] samples = ChunkPcm.AtPlaybackRate(format, audio.GetBuffer().AsMemory(0, checked((int)audio.Length)));
            owner.store.Write(new { op = "seal_chunk", id, endNs = EndNs, packets }, samples);
            Interlocked.Exchange(ref owner.savedThroughNs, EndNs);
            Interlocked.Increment(ref owner.sealedSegments);
        }

        private void Checkpoint()
        {
            owner.store.Write(new { op = "checkpoint", id, endNs = EndNs });
            Interlocked.Exchange(ref owner.accountedThroughNs, EndNs);
        }

        public void Dispose() => audio.Dispose();

        private sealed record PacketIndex(long StartNs, long EndNs, int Frames, long DevicePositionFrames, long Qpc100ns,
            int Flags, bool EstimatedTimestamp);
    }
}
