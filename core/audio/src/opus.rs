use std::io::{Read, Seek, Write};
use std::ptr::NonNull;

use ogg::PacketReader;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opusic_sys as sys;

use crate::{Error, MAX_CHANNELS, MAX_FRAMES, RATE, Result};

const FRAME: usize = 960;

struct Layout {
    channels: usize,
    coupled: i32,
    streams: i32,
    mapping: [u8; MAX_CHANNELS],
}

impl Layout {
    fn new(channels: usize) -> Result<Self> {
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(Error::InvalidMedia);
        }
        let coupled = i32::from(channels == 2);
        Ok(Self {
            channels,
            coupled,
            streams: channels as i32 - coupled,
            mapping: std::array::from_fn(|index| index as u8),
        })
    }

    fn head(&self, skip: u16, source_rate: u32) -> Vec<u8> {
        let mut header = b"OpusHead\x01".to_vec();
        header.push(self.channels as u8);
        header.extend(skip.to_le_bytes());
        header.extend(source_rate.to_le_bytes());
        header.extend([0, 0]); // Output gain
        if self.channels <= 2 {
            header.push(0);
        } else {
            // RFC 7845 family 255 preserves discrete channels in source order.
            header.extend([255, self.streams as u8, self.coupled as u8]);
            header.extend(&self.mapping[..self.channels]);
        }
        header
    }
}

struct Encoder(NonNull<sys::OpusMSEncoder>);

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { sys::opus_multistream_encoder_destroy(self.0.as_ptr()) };
    }
}

struct Decoder(NonNull<sys::OpusMSDecoder>);

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { sys::opus_multistream_decoder_destroy(self.0.as_ptr()) };
    }
}

fn checked(code: i32) -> Result<usize> {
    usize::try_from(code).map_err(|_| Error::InvalidMedia)
}

pub(crate) fn encode(
    mut output: impl Write,
    input: &[f32],
    channels: usize,
    source_rate: u32,
) -> Result<usize> {
    let layout = Layout::new(channels)?;
    let frames = input.len() / channels;
    if frames == 0
        || frames > MAX_FRAMES
        || !input.len().is_multiple_of(channels)
        || source_rate == 0
        || input.iter().any(|sample| !sample.is_finite())
    {
        return Err(Error::Invalid);
    }
    let mut status = 0;
    let encoder = Encoder(
        NonNull::new(unsafe {
            sys::opus_multistream_encoder_create(
                RATE as i32,
                channels as i32,
                layout.streams,
                layout.coupled,
                layout.mapping.as_ptr(),
                sys::OPUS_APPLICATION_AUDIO,
                &mut status,
            )
        })
        .ok_or(Error::InvalidMedia)?,
    );
    checked(status)?;
    checked(unsafe {
        sys::opus_multistream_encoder_ctl(
            encoder.0.as_ptr(),
            sys::OPUS_SET_COMPLEXITY_REQUEST,
            3i32,
        )
    })?;
    let mut skip = 0i32;
    checked(unsafe {
        sys::opus_multistream_encoder_ctl(
            encoder.0.as_ptr(),
            sys::OPUS_GET_LOOKAHEAD_REQUEST,
            &mut skip as *mut i32,
        )
    })?;
    let skip = u16::try_from(skip).map_err(|_| Error::InvalidMedia)?;
    let granule = frames + skip as usize;
    let packet_count = granule.div_ceil(FRAME);
    let mut writer = PacketWriter::new(&mut output);
    writer.write_packet(
        layout.head(skip, source_rate),
        1,
        PacketWriteEndInfo::EndPage,
        0,
    )?;
    let mut tags = b"OpusTags".to_vec();
    tags.extend(11u32.to_le_bytes());
    tags.extend(b"VoiceLedger");
    tags.extend(0u32.to_le_bytes());
    writer.write_packet(tags, 1, PacketWriteEndInfo::EndPage, 0)?;
    let mut samples = vec![0f32; FRAME * channels];
    let mut bytes = vec![0u8; 4096 * layout.streams as usize];
    for index in 0..packet_count {
        let start = index * samples.len();
        let end = (start + samples.len()).min(input.len());
        samples.fill(0.0);
        if start < end {
            samples[..end - start].copy_from_slice(&input[start..end]);
        }
        let length = checked(unsafe {
            sys::opus_multistream_encode_float(
                encoder.0.as_ptr(),
                samples.as_ptr(),
                FRAME as i32,
                bytes.as_mut_ptr(),
                bytes.len() as i32,
            )
        })?;
        let last = index + 1 == packet_count;
        writer.write_packet(
            bytes[..length].to_vec(),
            1,
            if last {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::EndPage
            },
            ((index + 1) * FRAME).min(granule) as u64,
        )?;
    }
    Ok(frames)
}

pub(crate) struct Decoded {
    pub channels: usize,
    pub source_rate: u32,
    pub samples: Vec<f32>,
}

pub(crate) fn decode(input: impl Read + Seek) -> Result<Decoded> {
    let mut reader = PacketReader::new(input);
    let mut packet = || reader.read_packet().map_err(|_| Error::InvalidMedia);
    let head = packet()?.ok_or(Error::InvalidMedia)?;
    if !head.first_in_stream() || head.last_in_stream() || head.data.len() < 19 {
        return Err(Error::InvalidMedia);
    }
    let channels = usize::from(head.data[9]);
    let layout = Layout::new(channels)?;
    let skip = u16::from_le_bytes([head.data[10], head.data[11]]);
    let source_rate = u32::from_le_bytes(
        head.data[12..16]
            .try_into()
            .map_err(|_| Error::InvalidMedia)?,
    );
    if head.data != layout.head(skip, source_rate) || source_rate == 0 {
        return Err(Error::InvalidMedia);
    }
    let tags = packet()?.ok_or(Error::InvalidMedia)?;
    if tags.stream_serial() != head.stream_serial()
        || tags.last_in_stream()
        || !tags.data.starts_with(b"OpusTags")
    {
        return Err(Error::InvalidMedia);
    }
    let mut status = 0;
    let decoder = Decoder(
        NonNull::new(unsafe {
            sys::opus_multistream_decoder_create(
                RATE as i32,
                channels as i32,
                layout.streams,
                layout.coupled,
                layout.mapping.as_ptr(),
                &mut status,
            )
        })
        .ok_or(Error::InvalidMedia)?,
    );
    checked(status)?;
    let mut filled = Vec::new();
    let mut buffer = vec![0f32; FRAME * channels];
    let total = loop {
        let page = packet()?.ok_or(Error::InvalidMedia)?;
        if page.stream_serial() != head.stream_serial() || page.first_in_stream() {
            return Err(Error::InvalidMedia);
        }
        let frames = checked(unsafe {
            sys::opus_multistream_decode_float(
                decoder.0.as_ptr(),
                page.data.as_ptr(),
                i32::try_from(page.data.len()).map_err(|_| Error::InvalidMedia)?,
                buffer.as_mut_ptr(),
                FRAME as i32,
                0,
            )
        })?;
        if frames != FRAME || filled.len() / channels >= MAX_FRAMES + usize::from(skip) {
            return Err(Error::InvalidMedia);
        }
        filled.extend_from_slice(&buffer);
        let total_frames = filled.len() / channels;
        let end = usize::try_from(page.absgp_page()).map_err(|_| Error::InvalidMedia)?;
        if page.last_in_stream() {
            if end <= usize::from(skip)
                || end <= total_frames - FRAME
                || end > total_frames
                || end - usize::from(skip) > MAX_FRAMES
                || packet()?.is_some()
            {
                return Err(Error::InvalidMedia);
            }
            break end;
        }
        if end != total_frames {
            return Err(Error::InvalidMedia);
        }
    };
    filled.truncate(total * channels);
    filled.drain(..usize::from(skip) * channels);
    Ok(Decoded {
        channels,
        source_rate,
        samples: filled,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn opus_encodes_a_partial_final_frame_and_discrete_channels() {
        for channels in [1, 2, 6] {
            let input: Vec<f32> = (0..2501 * channels)
                .map(|index| {
                    if index % channels == 0 {
                        ((index as f32 / channels as f32) * 0.05).sin() * 0.2
                    } else {
                        0.0
                    }
                })
                .collect();
            let mut bytes = Vec::new();
            assert_eq!(encode(&mut bytes, &input, channels, 44_100).unwrap(), 2501);
            let decoded = decode(Cursor::new(&bytes)).unwrap();
            assert_eq!(decoded.channels, channels);
            assert_eq!(decoded.source_rate, 44_100);
            assert_eq!(decoded.samples.len(), input.len());
            assert!(
                decoded
                    .samples
                    .iter()
                    .step_by(channels)
                    .any(|sample| sample.abs() > 0.05)
            );
            bytes.truncate(bytes.len() - 2);
            assert!(decode(Cursor::new(bytes)).is_err());
        }
    }
}
