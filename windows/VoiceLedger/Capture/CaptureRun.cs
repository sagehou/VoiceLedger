using System;
using System.Buffers;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Channels;
using System.Threading.Tasks;
using NAudio.CoreAudioApi;
using VoiceLedger.Core;

namespace VoiceLedger.Capture;

internal sealed class CaptureRun
{
    internal const int QueuePackets = 32;
    private const int MaxPacketBytes = 1024 * 1024;
    private readonly CaptureClock clock;
    private readonly TrackFiles files;
    private readonly uint epoch;
    private readonly OpenCaptureInput open;
    private readonly PendingGap? prefix;
    private readonly string? expectedFormat;
    private readonly ManualResetEvent wake = new(false);
    private readonly TaskCompletionSource<EpochStart> started = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly Channel<CapturedPacket> queue = Channel.CreateBounded<CapturedPacket>(new BoundedChannelOptions(QueuePackets)
    {
        SingleWriter = true, SingleReader = true, FullMode = BoundedChannelFullMode.Wait
    });
    private CaptureStop? stop;
    private long lastEmptyNs;

    internal CaptureRun(CaptureClock clock, TrackFiles files, uint epoch, PendingGap? prefix, OpenCaptureInput open, string? expectedFormat = null)
    {
        this.clock = clock;
        this.files = files;
        this.epoch = epoch;
        this.prefix = prefix;
        this.open = open;
        this.expectedFormat = expectedFormat;
        var producer = Task.Factory.StartNew(Produce, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
        Completion = Task.Run(() => WriteAsync(producer));
    }

    internal Task<EpochStart> Started => started.Task;
    internal Task<EpochResult> Completion { get; }
    internal int QueuedPackets => queue.Reader.Count;
    private CaptureStop? StopRequest => Volatile.Read(ref stop);

    internal void RequestStop(CaptureEnd reason)
    {
        var requested = new CaptureStop(reason, clock.NowNs);
        CaptureStop? previous;
        do
        {
            previous = StopRequest;
            if (previous is { Graceful: false } || previous is not null && requested.Graceful)
            {
                return;
            }
        } while (!ReferenceEquals(Interlocked.CompareExchange(ref stop, requested, previous), previous));
        try { wake.Set(); }
        catch (ObjectDisposedException) { }
    }

    private void Produce()
    {
        try
        {
            using ICaptureInput input = open(files.Endpoint, RequestStop);
            var format = input.Format;
            if (format.BlockAlign <= 0 || format.SampleRate <= 0 || input.BufferFrames <= 0)
            {
                throw new IOException("Invalid endpoint format.");
            }
            try { _ = ChunkPcm.SampleFormat(format); }
            catch (InvalidDataException)
            {
                RequestStop(CaptureEnd.UnsupportedFormat);
                return;
            }
            if (expectedFormat is not null && CaptureFormat.Signature(format) != expectedFormat)
            {
                RequestStop(CaptureEnd.FormatChanged);
                return;
            }
            if (StopRequest is not null)
            {
                return;
            }

            long startNs = clock.NowNs;
            input.Start();
            var start = new EpochStart(startNs, format, input.BufferFrames);
            started.TrySetResult(start);
            var ready = new[] { wake, input.PacketReady };
            PacketConsumer offer = (data, stamp) => Offer(data, stamp, format.BlockAlign, format.SampleRate);
            long nextCheckpointNs = startNs + 1_000_000_000;
            try
            {
                while (StopRequest is null)
                {
                    while (StopRequest is null && input.TryRead(offer)) { }
                    if (StopRequest is null)
                    {
                        long observed = clock.NowNs;
                        Interlocked.Exchange(ref lastEmptyNs, observed);
                        if (observed >= nextCheckpointNs)
                        {
                            // A zero-frame checkpoint lets the writer seal audio during idle periods.
                            queue.Writer.TryWrite(new CapturedPacket(default, observed, null, 0));
                            nextCheckpointNs = observed + 1_000_000_000;
                        }
                        WaitHandle.WaitAny(ready, 100);
                    }
                }
            }
            finally
            {
                input.Stop();
            }
            while (StopRequest is { Graceful: true } && input.TryRead(offer)) { }
        }
        catch (COMException failure) when ((uint)failure.ErrorCode == 0x88890004)
        {
            RequestStop(CaptureEnd.EndpointLost);
        }
        catch (COMException failure) when ((uint)failure.ErrorCode == 0x88890026)
        {
            RequestStop(CaptureEnd.SystemSuspend);
        }
        catch (Exception)
        {
            RequestStop(CaptureEnd.SourceFailure);
        }
        finally
        {
            started.TrySetException(new IOException("The selected audio source did not start."));
            queue.Writer.TryComplete();
        }
    }

    private void Offer(nint data, PacketStamp stamp, int blockAlign, int sampleRate)
    {
        long bytes = (long)stamp.Frames * blockAlign;
        if (bytes <= 0 || bytes > MaxPacketBytes || stamp.Frames > (long)sampleRate * 10)
        {
            RequestStop(CaptureEnd.SourceFailure);
            return;
        }
        long observed = clock.NowNs;
        byte[]? buffer = null;
        try
        {
            if ((stamp.Flags & AudioClientBufferFlags.Silent) == 0)
            {
                if (data == 0)
                {
                    RequestStop(CaptureEnd.SourceFailure);
                    return;
                }
                buffer = ArrayPool<byte>.Shared.Rent((int)bytes);
                Marshal.Copy(data, buffer, 0, (int)bytes);
            }
            if (queue.Writer.TryWrite(new CapturedPacket(stamp, observed, buffer, (int)bytes)))
            {
                buffer = null;
            }
            else
            {
                RequestStop(CaptureEnd.QueueOverflow);
            }
        }
        finally
        {
            ReturnBuffer(buffer);
        }
    }

    private async Task<EpochResult> WriteAsync(Task producer)
    {
        TrackFiles.Segment? segment = null;
        CaptureInterval? pendingInterval = null;
        bool opened = false;
        try
        {
            EpochStart start;
            try { start = await Started.ConfigureAwait(false); }
            catch (IOException)
            {
                var failed = StopRequest ?? new CaptureStop(CaptureEnd.SourceFailure, clock.NowNs);
                return new EpochResult(failed.Reason, failed.AtNs, files.SavedThroughNs, false);
            }
            files.BeginEpoch(epoch, start, prefix);
            opened = true;
            var timeline = new PacketTimeline(files.Endpoint.Source, start.Format.SampleRate, start.AtNs);
            await foreach (CapturedPacket packet in queue.Reader.ReadAllAsync().ConfigureAwait(false))
            {
                try
                {
                    if (packet.Stamp.Frames == 0)
                    {
                        if (packet.ObservedAtNs - timeline.EndNs >= 1_000_000_000) SealSegment();
                        continue;
                    }
                    var placed = timeline.Place(packet.Stamp,
                        clock.MapQpc(packet.Stamp.Qpc100ns, packet.ObservedAtNs), packet.ObservedAtNs);
                    if (placed.Before is CaptureInterval before)
                    {
                        SealSegment();
                        AddInterval(before);
                    }
                    if (packet.Buffer is null)
                    {
                        SealSegment();
                        AddInterval(new CaptureInterval("silence", placed.StartNs, placed.EndNs));
                    }
                    else
                    {
                        FlushInterval();
                        if (segment is not null && !segment.CanAdd(packet, placed))
                        {
                            SealSegment();
                        }
                        segment ??= files.StartSegment(epoch, start.Format, placed);
                        segment.Write(packet, placed);
                    }
                }
                finally
                {
                    ReturnBuffer(packet.Buffer);
                }
            }
            await producer.ConfigureAwait(false);
            SealSegment();
            var end = StopRequest ?? new CaptureStop(CaptureEnd.SourceFailure, clock.NowNs);
            long bufferNs = CaptureClock.FrameDuration(start.BufferFrames, start.Format.SampleRate);
            long idleBoundary = end.Graceful ? end.AtNs : Math.Min(end.AtNs, Interlocked.Read(ref lastEmptyNs) - bufferNs);
            if (timeline.ObserveIdleTo(idleBoundary) is CaptureInterval idle)
            {
                AddInterval(idle);
            }
            long endNs = Math.Max(end.AtNs, timeline.EndNs);
            if (!end.Graceful)
            {
                AddInterval(new CaptureInterval("gap", timeline.EndNs, endNs, CaptureReason.Code(end.Reason)));
            }
            FlushInterval();
            var result = new EpochResult(end.Reason, endNs, timeline.EndNs, true);
            files.EndEpoch(epoch, result);
            return result;
        }
        catch (Exception failure)
        {
            CaptureEnd reason = failure is StorageReserveException ? CaptureEnd.StorageExhausted : CaptureEnd.StorageFailure;
            RequestStop(reason);
            var result = new EpochResult(reason, clock.NowNs, files.AccountedThroughNs, opened);
            if (opened)
            {
                try { files.EndEpoch(epoch, result); }
                catch (Exception metadataFailure) when (metadataFailure is IOException or CoreException) { }
            }
            return result;
        }
        finally
        {
            await producer.ConfigureAwait(false);
            while (queue.Reader.TryRead(out CapturedPacket queued))
            {
                ReturnBuffer(queued.Buffer);
            }
            try { segment?.Dispose(); }
            catch (IOException) { }
            wake.Dispose();
        }

        void SealSegment()
        {
            if (segment is null)
            {
                return;
            }
            using var completed = segment;
            segment = null;
            completed.Seal();
        }

        void AddInterval(CaptureInterval interval)
        {
            if (interval.EndNs <= interval.StartNs)
            {
                return;
            }
            if (pendingInterval is CaptureInterval prior && prior.Kind == interval.Kind
                && prior.Cause == interval.Cause && prior.EndNs == interval.StartNs
                && interval.EndNs - prior.StartNs <= 10_000_000_000L)
            {
                pendingInterval = prior with { EndNs = interval.EndNs };
                return;
            }
            FlushInterval();
            pendingInterval = interval;
        }

        void FlushInterval()
        {
            if (pendingInterval is CaptureInterval interval)
            {
                files.AddInterval(epoch, interval);
                pendingInterval = null;
            }
        }
    }

    private static void ReturnBuffer(byte[]? buffer)
    {
        if (buffer is not null)
        {
            ArrayPool<byte>.Shared.Return(buffer, clearArray: true);
        }
    }
}
