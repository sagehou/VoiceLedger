using System;
using NAudio.CoreAudioApi;

namespace VoiceLedger.Capture;

internal sealed class PacketTimeline(CaptureSource source, int sampleRate, long startNs)
{
    private long? previousPositionEnd;
    private bool hasPacket;
    internal long EndNs { get; private set; } = startNs;

    internal PlacedPacket Place(PacketStamp stamp, long? hardwareStartNs, long observedNs)
    {
        long duration = CaptureClock.FrameDuration(stamp.Frames, sampleRate);
        long samplePeriod = Math.Max(100, CaptureClock.FrameDuration(1, sampleRate));
        bool estimated = hardwareStartNs is null || (stamp.Flags & AudioClientBufferFlags.TimestampError) != 0;
        if (!estimated && hardwareStartNs < EndNs - samplePeriod)
        {
            estimated = true;
        }
        long placedStart = Math.Max(EndNs, estimated ? observedNs - duration : hardwareStartNs!.Value);
        if (placedStart - EndNs <= samplePeriod)
        {
            placedStart = EndNs;
        }

        bool reliablePosition = !estimated && stamp.Position >= 0 && stamp.Position <= long.MaxValue - stamp.Frames;
        bool flaggedLoss = hasPacket && (stamp.Flags & AudioClientBufferFlags.DataDiscontinuity) != 0;
        bool missingMicFrames = source == CaptureSource.Mic && reliablePosition && previousPositionEnd is long expected
            && stamp.Position > expected;
        CaptureInterval? before = null;
        if (placedStart > EndNs)
        {
            if (flaggedLoss || missingMicFrames)
            {
                before = new CaptureInterval("gap", EndNs, placedStart, "source_discontinuity");
            }
            else if (source == CaptureSource.System && !estimated)
            {
                before = new CaptureInterval("silence", EndNs, placedStart);
            }
        }
        EndNs = checked(placedStart + duration);
        previousPositionEnd = reliablePosition ? stamp.Position + stamp.Frames : null;
        hasPacket = true;
        return new PlacedPacket(placedStart, EndNs, estimated, before);
    }

    internal CaptureInterval? ObserveIdleTo(long endNs)
    {
        if (source != CaptureSource.System || endNs <= EndNs)
        {
            return null;
        }
        var silence = new CaptureInterval("silence", EndNs, endNs);
        EndNs = endNs;
        return silence;
    }
}
