using System;
using NAudio.Wave;
using VoiceLedger.Core;

namespace VoiceLedger.Capture;

internal sealed class CommittedPlayback(Func<long, PlaybackChunk> seek, PlaybackChunk first) : IWaveProvider
{
    private PlaybackChunk chunk = first;
    private int byteOffset;
    private bool ended;

    public WaveFormat WaveFormat { get; } = first.Channels <= 2
        ? WaveFormat.CreateIeeeFloatWaveFormat(AudioStore.PlaybackRate, first.Channels)
        : new WaveFormatExtensible(AudioStore.PlaybackRate, 32, first.Channels);

    public int Read(byte[] buffer, int offset, int count)
    {
        int written = 0;
        while (!ended && written < count)
        {
            int available = chunk.Samples.Length * sizeof(float) - byteOffset;
            if (available == 0)
            {
                try
                {
                    PlaybackChunk next = seek(chunk.NextNs);
                    if (next.Channels != WaveFormat.Channels)
                    {
                        ended = true;
                        break;
                    }
                    chunk = next;
                    byteOffset = 0;
                }
                catch (CoreException failure) when (failure.Code == AudioStore.Unavailable || failure.Code == NativeCore.SpeechNoAudio)
                {
                    ended = true;
                }
                continue;
            }

            int copied = Math.Min(available, count - written);
            Buffer.BlockCopy(chunk.Samples, byteOffset, buffer, offset + written, copied);
            byteOffset += copied;
            written += copied;
        }
        return written;
    }
}
