//! Recordings (ADR 0009 decision 2): one Ogg Opus stream (RFC 7845), 48 kHz mono, 20 ms
//! frames. The stream is handed out in chunks that end at page boundaries, so every chunk can be
//! stored durably on its own and the concatenated chunks are the stream again; a crash loses at
//! most the audio not yet in a chunk.

use crate::{AudioError, CHUNK_MS, FRAME, MAX_DECODED_SAMPLES, Pcm, RATE};
use ogg::{PacketReader, PacketWriteEndInfo, PacketWriter};

const BITRATE: i32 = 32_000;
/// Largest Opus packet (RFC 6716) and largest frame (120 ms at 48 kHz, two channels).
const MAX_PACKET: usize = 1275 * 3;
const MAX_FRAME_SAMPLES: usize = 5760 * 2;

fn oerr(e: opus::Error) -> AudioError {
    AudioError::Opus(e.to_string())
}
fn ioerr(e: std::io::Error) -> AudioError {
    AudioError::Ogg(e.to_string())
}

/// Encodes a growing recording.
pub struct StreamEncoder {
    enc: opus::Encoder,
    w: PacketWriter<'static, Vec<u8>>,
    serial: u32,
    pending: Vec<f32>,
    /// Real samples taken so far, samples encoded (whole frames) and at the chunk start.
    taken: u64,
    encoded: u64,
    chunk_start: u64,
    pre_skip: u64,
    packet: Vec<u8>,
}

impl StreamEncoder {
    /// A new stream; `serial` identifies it in the Ogg pages.
    pub fn new(serial: u32) -> Result<StreamEncoder, AudioError> {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).map_err(oerr)?;
        enc.set_bitrate(opus::Bitrate::Bits(BITRATE)).map_err(oerr)?;
        let pre_skip = u64::try_from(enc.get_lookahead().map_err(oerr)?).unwrap_or(0);
        let mut w = PacketWriter::new(Vec::new());
        // RFC 7845 §5.1 identification header and §5.2 comment header, each on its own page.
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(1); // channels
        head.extend_from_slice(&u16::try_from(pre_skip).unwrap_or(0).to_le_bytes());
        head.extend_from_slice(&RATE.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family
        w.write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(ioerr)?;
        let vendor = b"vibenote";
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&u32::try_from(vendor.len()).unwrap_or(0).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes());
        w.write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(ioerr)?;
        Ok(StreamEncoder {
            enc,
            w,
            serial,
            pending: Vec::with_capacity(FRAME),
            taken: 0,
            encoded: 0,
            chunk_start: 0,
            pre_skip,
            packet: vec![0u8; MAX_PACKET],
        })
    }

    /// Real samples taken so far, as milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.taken
            .saturating_mul(1000)
            .checked_div(u64::from(RATE))
            .unwrap_or(0)
    }

    fn frame(&mut self, frame: &[f32], end: PacketWriteEndInfo, granule: u64) -> Result<(), AudioError> {
        let n = self.enc.encode_float(frame, &mut self.packet).map_err(oerr)?;
        let data = self.packet.get(..n).unwrap_or(&[]).to_vec();
        self.w.write_packet(data, self.serial, end, granule).map_err(ioerr)?;
        self.encoded = self.encoded.saturating_add(FRAME as u64);
        Ok(())
    }

    /// Adds samples (48 kHz mono); returns the chunks completed by them (whole pages).
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<Vec<u8>>, AudioError> {
        let mut out = Vec::new();
        let chunk_samples = CHUNK_MS.saturating_mul(u64::from(RATE)) / 1000;
        let mut rest = samples;
        while !rest.is_empty() {
            let need = FRAME.saturating_sub(self.pending.len());
            let (take, after) = rest.split_at(need.min(rest.len()));
            self.pending.extend_from_slice(take);
            self.taken = self.taken.saturating_add(take.len() as u64);
            rest = after;
            if self.pending.len() < FRAME {
                break;
            }
            let frame = std::mem::take(&mut self.pending);
            let granule = self.pre_skip.saturating_add(self.encoded).saturating_add(FRAME as u64);
            let end_chunk = self
                .encoded
                .saturating_add(FRAME as u64)
                .saturating_sub(self.chunk_start)
                >= chunk_samples;
            let end = if end_chunk {
                PacketWriteEndInfo::EndPage
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            self.frame(&frame, end, granule)?;
            self.pending = frame;
            self.pending.clear();
            if end_chunk {
                self.chunk_start = self.encoded;
                out.push(std::mem::take(self.w.inner_mut()));
            }
        }
        Ok(out)
    }

    /// Ends the stream: the last chunk, and the recording's length in milliseconds. The
    /// encoder's look-ahead is flushed with silence and trimmed again by the end granule.
    pub fn finish(mut self) -> Result<(Vec<u8>, u64), AudioError> {
        let real = self.taken;
        let total = real.saturating_add(self.pre_skip);
        let last_granule = total;
        loop {
            self.pending.resize(FRAME, 0.0);
            let frame = std::mem::take(&mut self.pending);
            let done = self.encoded.saturating_add(FRAME as u64) >= total;
            let end = if done {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            self.frame(&frame, end, last_granule)?;
            if done {
                break;
            }
        }
        let ms = real.saturating_mul(1000).checked_div(u64::from(RATE)).unwrap_or(0);
        Ok((std::mem::take(self.w.inner_mut()), ms))
    }
}

/// Decodes an Ogg Opus stream (the concatenated chunks of a recording, or an imported `.opus`
/// file). A stream cut short (crash) decodes up to its last complete page.
pub fn decode(bytes: &[u8]) -> Result<Pcm, AudioError> {
    let mut r = PacketReader::new(std::io::Cursor::new(bytes));
    let head = match r.read_packet() {
        Ok(Some(p)) => p.data,
        Ok(None) => return Err(AudioError::Format("empty Ogg stream".into())),
        Err(e) => return Err(AudioError::Ogg(e.to_string())),
    };
    if head.get(..8) != Some(b"OpusHead".as_slice()) || head.len() < 19 {
        return Err(AudioError::Format("not an Opus stream".into()));
    }
    let channels = head.get(9).copied().unwrap_or(0);
    let pre_skip = u64::from(u16::from_le_bytes([
        head.get(10).copied().unwrap_or(0),
        head.get(11).copied().unwrap_or(0),
    ]));
    if !(1..=2).contains(&channels) || head.get(18).copied().unwrap_or(1) != 0 {
        return Err(AudioError::Format("only mono and stereo Opus are supported".into()));
    }
    let ch = if channels == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    };
    let mut dec = opus::Decoder::new(RATE, ch).map_err(oerr)?;
    let mut samples: Vec<f32> = Vec::new();
    let mut frame = vec![0f32; MAX_FRAME_SAMPLES];
    let mut last_granule: Option<u64> = None;
    let mut first = true;
    // Until the end, or a damaged/cut tail: keep what was decoded.
    while let Ok(Some(p)) = r.read_packet() {
        if first {
            first = false;
            if p.data.get(..8) == Some(b"OpusTags".as_slice()) {
                continue;
            }
        }
        let n = dec.decode_float(&p.data, &mut frame, false).map_err(oerr)?;
        let n = n.saturating_mul(usize::from(channels));
        samples.extend_from_slice(frame.get(..n).unwrap_or(&[]));
        if samples.len() > MAX_DECODED_SAMPLES {
            return Err(AudioError::TooLong);
        }
        if p.last_in_page() {
            last_granule = Some(p.absgp_page());
        }
    }
    let ch = usize::from(channels);
    let frames = samples.len().checked_div(ch).unwrap_or(0);
    let skip = usize::try_from(pre_skip).unwrap_or(usize::MAX).min(frames);
    // The end granule (pre-skip included) says how many frames are real.
    let end = last_granule
        .and_then(|g| usize::try_from(g).ok())
        .map_or(frames, |g| g.min(frames))
        .max(skip);
    let samples = samples
        .get(skip.saturating_mul(ch)..end.saturating_mul(ch))
        .unwrap_or(&[])
        .to_vec();
    Ok(Pcm {
        rate: RATE,
        channels: u16::from(channels),
        samples,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
mod tests {
    use super::*;

    #[test]
    fn chunks_concatenate_to_the_stream_and_decode_to_the_recorded_length() {
        let mut e = StreamEncoder::new(7).unwrap();
        let tone = crate::tone::samples(0, 25 * RATE as usize + 123);
        let mut chunks = Vec::new();
        // Odd push sizes, as a capture callback delivers them.
        for part in tone.chunks(1777) {
            chunks.extend(e.push(part).unwrap());
        }
        assert_eq!(chunks.len(), 2, "two full 10 s chunks so far");
        let (last, ms) = e.finish().unwrap();
        chunks.push(last);
        assert_eq!(ms, 25_002);
        for c in &chunks {
            assert_eq!(&c[..4], b"OggS", "every chunk starts with a page");
        }
        let all: Vec<u8> = chunks.concat();
        let pcm = decode(&all).unwrap();
        assert_eq!(pcm.samples.len(), tone.len(), "pre-skip and end trimming are exact");
        // Lossy, but aligned: the decoded tone correlates with the input.
        let (a, b) = (&tone[48_000..96_000], &pcm.samples[48_000..96_000]);
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(dot / (na * nb) > 0.9, "correlation {}", dot / (na * nb));
        // A crash after the first chunk: that chunk alone decodes to its 10 s.
        let first = decode(&chunks[0]).unwrap();
        assert!(
            (first.duration_ms() as i64 - 10_000).abs() <= 25,
            "{}",
            first.duration_ms()
        );
    }

    #[test]
    fn hostile_streams_are_errors_or_empty() {
        for junk in [
            &b""[..],
            b"OggS",
            b"OggS\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0garbage",
            &[0xffu8; 4096][..],
        ] {
            if let Ok(p) = decode(junk) {
                assert!(p.samples.is_empty());
            }
        }
        // Truncated anywhere: never panics.
        let mut e = StreamEncoder::new(1).unwrap();
        e.push(&crate::tone::samples(0, 48_000)).unwrap();
        let (bytes, _) = e.finish().unwrap();
        for cut in (0..bytes.len()).step_by(97) {
            let _ = decode(&bytes[..cut]);
        }
    }
}
