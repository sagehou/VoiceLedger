using System;
using System.Buffers.Binary;
using System.IO;
using NAudio.Wave;
using NAudio.Wave.SampleProviders;
using VoiceLedger.Core;

namespace VoiceLedger.Capture;

internal static class ChunkPcm
{
    internal static string SampleFormat(WaveFormat source)
    {
        WaveFormat format = Standard(source);
        if (format.Channels is < 1 or > AudioStore.MaxChannels || format.SampleRate <= 0
            || format.BlockAlign != source.BlockAlign || format.BlockAlign != format.Channels * (format.BitsPerSample / 8))
            throw new InvalidDataException("Unsupported capture format.");
        return (format.Encoding, format.BitsPerSample) switch
        {
            (WaveFormatEncoding.Pcm, 8) => "pcm_u8",
            (WaveFormatEncoding.Pcm, 16) => "pcm_s16le",
            (WaveFormatEncoding.Pcm, 24) => "pcm_s24le",
            (WaveFormatEncoding.Pcm, 32) => "pcm_s32le",
            (WaveFormatEncoding.IeeeFloat, 32) => "pcm_f32le",
            _ => throw new InvalidDataException("Unsupported capture format.")
        };
    }

    internal static float[] AtPlaybackRate(WaveFormat source, ReadOnlyMemory<byte> captured)
    {
        string format = SampleFormat(source);
        if (captured.IsEmpty || captured.Length % source.BlockAlign != 0)
            throw new InvalidDataException("Invalid captured PCM size.");
        var input = new PcmSamples(source, captured, format);
        int frames = captured.Length / source.BlockAlign;
        int outputFrames = checked((int)(((long)frames * AudioStore.PlaybackRate + source.SampleRate - 1) / source.SampleRate));
        var output = new float[checked(outputFrames * source.Channels)];
        ISampleProvider provider = source.SampleRate == AudioStore.PlaybackRate
            ? input : new WdlResamplingSampleProvider(input, AudioStore.PlaybackRate);
        int written = 0;
        while (written < output.Length)
        {
            int read = provider.Read(output, written, output.Length - written);
            if (read <= 0) throw new InvalidDataException("Recording resampling ended early.");
            written += read;
        }
        return output;
    }

    private static WaveFormat Standard(WaveFormat format) =>
        format is WaveFormatExtensible extensible ? extensible.ToStandardWaveFormat() : format;

    private sealed class PcmSamples(WaveFormat source, ReadOnlyMemory<byte> bytes, string format) : ISampleProvider
    {
        private readonly int sampleBytes = source.BitsPerSample / 8;
        private int position;
        public WaveFormat WaveFormat { get; } = WaveFormat.CreateIeeeFloatWaveFormat(source.SampleRate, source.Channels);

        public int Read(float[] buffer, int offset, int count)
        {
            int copied = Math.Min((bytes.Length - position) / sampleBytes, count);
            ReadOnlySpan<byte> data = bytes.Span;
            for (int index = 0; index < copied; index++)
            {
                ReadOnlySpan<byte> sample = data.Slice(position, sampleBytes);
                buffer[offset + index] = format switch
                {
                    "pcm_u8" => (sample[0] - 128) / 128f,
                    "pcm_s16le" => BinaryPrimitives.ReadInt16LittleEndian(sample) / 32768f,
                    "pcm_s24le" => ((sample[0] << 8) | (sample[1] << 16) | (sample[2] << 24)) / 2147483648f,
                    "pcm_s32le" => BinaryPrimitives.ReadInt32LittleEndian(sample) / 2147483648f,
                    _ => BinaryPrimitives.ReadSingleLittleEndian(sample),
                };
                position += sampleBytes;
            }
            // The resampler reads beyond the source for its filter tail. The caller trims to
            // the exact rescaled frame count, so no padding extends the capture timeline.
            Array.Clear(buffer, offset + copied, count - copied);
            return count;
        }
    }
}
