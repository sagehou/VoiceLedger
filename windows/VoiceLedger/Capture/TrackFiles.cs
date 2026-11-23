using System;
using System.Collections.Generic;
using System.IO;
using System.Security.Cryptography;
using System.Text.Json;
using System.Threading;
using NAudio.Utils;
using NAudio.Wave;

namespace VoiceLedger.Capture;

internal readonly record struct PendingGap(uint Epoch, long StartNs, string Cause);

internal sealed class TrackFiles
{
    private static readonly JsonSerializerOptions Json = new() { PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower };
    private readonly string directory;
    private readonly string journal;
    private long nextSequence;
    private long savedThroughNs;
    private int sealedSegments;
    private int lossIntervals;

    internal TrackFiles(string sessionDirectory, CaptureEndpoint endpoint)
    {
        Id = Guid.NewGuid();
        Endpoint = endpoint;
        directory = Path.Combine(sessionDirectory, $"{endpoint.Source.ToString().ToLowerInvariant()}-{Id:D}");
        Directory.CreateDirectory(directory);
        journal = Path.Combine(directory, "timeline.jsonl");
        Append(new { kind = "track", track_id = Id, source = endpoint.Source.ToString(), endpoint_id = endpoint.Id,
            endpoint_name = endpoint.Name, clock = "windows_qpc" });
    }

    internal Guid Id { get; }
    internal CaptureEndpoint Endpoint { get; }
    internal int SealedSegments => Volatile.Read(ref sealedSegments);
    internal int LossIntervals => Volatile.Read(ref lossIntervals);
    internal long SavedThroughNs => Interlocked.Read(ref savedThroughNs);

    internal void BeginEpoch(uint epoch, EpochStart start, PendingGap? prefix)
    {
        if (prefix is PendingGap gap)
        {
            AddInterval(gap.Epoch, new CaptureInterval("gap", gap.StartNs, start.AtNs, gap.Cause));
        }
        Append(new { kind = "stream_started", source_epoch = epoch, start_ns = start.AtNs,
            sample_rate_hz = start.Format.SampleRate, channels = start.Format.Channels,
            bits_per_sample = start.Format.BitsPerSample, block_align = start.Format.BlockAlign,
            mix_format = start.Format.ToString(), wasapi_buffer_frames = start.BufferFrames });
    }

    internal void AddInterval(uint epoch, CaptureInterval interval)
    {
        if (interval.EndNs > interval.StartNs)
        {
            Append(new { kind = interval.Kind, id = Guid.NewGuid(), source_epoch = epoch,
                start_ns = interval.StartNs, end_ns = interval.EndNs, cause = interval.Cause });
            if (interval.Kind == "gap" && interval.Cause != "paused")
            {
                Interlocked.Increment(ref lossIntervals);
            }
        }
    }

    internal void EndEpoch(uint epoch, EpochResult result) => Append(new
    {
        kind = "stream_ended", source_epoch = epoch, reason = CaptureReason.Code(result.Reason),
        end_ns = result.EndNs, covered_until_ns = result.CoveredUntilNs,
        saved_through_ns = SavedThroughNs
    });

    internal WaveSegment StartSegment(uint epoch, WaveFormat format)
    {
        CheckReserve(directory);
        return new WaveSegment(this, directory, epoch, nextSequence++, format);
    }

    internal static void CheckReserve(string path)
    {
        string root = Path.GetPathRoot(Path.GetFullPath(path)) ?? throw new IOException("Recording volume is unavailable.");
        const long reserve = 64L * 1024 * 1024;
        if (new DriveInfo(root).AvailableFreeSpace < reserve)
        {
            throw new IOException("The recording volume is below the prototype storage reserve.");
        }
    }

    private void Append<T>(T value)
    {
        byte[] record = JsonSerializer.SerializeToUtf8Bytes(value, Json);
        using var file = new FileStream(journal, FileMode.Append, FileAccess.Write, FileShare.Read);
        file.Write(record);
        file.WriteByte((byte)'\n');
        file.Flush(flushToDisk: true);
    }

    internal sealed class WaveSegment : IDisposable
    {
        private readonly TrackFiles owner;
        private readonly Guid id = Guid.NewGuid();
        private readonly uint epoch;
        private readonly long sequence;
        private readonly WaveFormat format;
        private readonly string partial;
        private readonly string complete;
        private readonly List<PacketIndex> packets = new();
        private FileStream? file;
        private WaveFileWriter? wave;
        private long frames;
        private long bytes;

        internal WaveSegment(TrackFiles owner, string directory, uint epoch, long sequence, WaveFormat format)
        {
            this.owner = owner;
            this.epoch = epoch;
            this.sequence = sequence;
            this.format = format;
            complete = Path.Combine(directory, $"{sequence:000000}-{id:D}.wav");
            partial = complete + ".partial";
            file = new FileStream(partial, FileMode.CreateNew, FileAccess.ReadWrite, FileShare.None);
            try
            {
                wave = new WaveFileWriter(new IgnoreDisposeStream(file), format);
            }
            catch
            {
                file.Dispose();
                throw;
            }
        }

        internal bool CanAdd(int packetFrames) => frames + packetFrames <= (long)format.SampleRate * 10 && packets.Count < 512;

        internal void Write(CapturedPacket packet, PlacedPacket place)
        {
            wave!.Write(packet.Buffer!, 0, packet.Length);
            packets.Add(new PacketIndex(place.StartNs, place.EndNs, bytes, packet.Stamp.Frames, packet.Stamp.Position,
                packet.Stamp.Qpc100ns, packet.ObservedAtNs, (int)packet.Stamp.Flags, place.Estimated));
            frames += packet.Stamp.Frames;
            bytes += packet.Length;
        }

        internal void Seal()
        {
            wave!.Dispose();
            wave = null;
            FileStream current = file!;
            current.Flush(flushToDisk: true);
            long length = current.Length;
            current.Position = 0;
            string sha256 = Convert.ToHexStringLower(SHA256.HashData(current));
            current.Dispose();
            file = null;
            File.Move(partial, complete);
            owner.Append(new { kind = "wav_segment", id, source_epoch = epoch, sequence,
                start_ns = packets[0].StartNs, end_ns = packets[^1].EndNs, frames, byte_size = length, sha256,
                filename = Path.GetFileName(complete), packets });
            Interlocked.Exchange(ref owner.savedThroughNs, packets[^1].EndNs);
            Interlocked.Increment(ref owner.sealedSegments);
        }

        public void Dispose()
        {
            try { wave?.Dispose(); }
            finally { file?.Dispose(); }
        }

        private sealed record PacketIndex(long StartNs, long EndNs, long PcmOffsetBytes, int Frames,
            long DevicePositionFrames, long Qpc100ns, long ObservedAtNs, int Flags, bool EstimatedTimestamp);
    }
}
