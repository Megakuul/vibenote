//! Imported audio files (R-AUDIO-001): WAV, FLAC, MP3 and Ogg Vorbis via symphonia (MPL-2.0,
//! owner-approved, unmodified), Ogg Opus via libopus. Files are hostile input: size and decoded
//! length are bounded, and a panic inside a decoder is contained and reported as an error.

use crate::{AudioError, MAX_DECODED_SAMPLES, Pcm};

/// Largest audio file accepted.
pub const MAX_FILE: u64 = 1 << 30;

/// True for an Ogg stream whose first packet is an Opus header.
fn is_ogg_opus(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS") && bytes.get(..512).is_some_and(|h| h.windows(8).any(|w| w == b"OpusHead"))
}

/// Decodes a whole file.
pub fn decode(bytes: &[u8]) -> Result<Pcm, AudioError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE {
        return Err(AudioError::TooLong);
    }
    if is_ogg_opus(bytes) {
        return crate::opusogg::decode(bytes);
    }
    let owned = bytes.to_vec();
    std::panic::catch_unwind(move || decode_symphonia(owned))
        .unwrap_or_else(|_| Err(AudioError::Format("the decoder failed on this file".into())))
}

fn fmt(e: impl std::fmt::Display) -> AudioError {
    AudioError::Format(e.to_string())
}

fn decode_symphonia(bytes: Vec<u8>) -> Result<Pcm, AudioError> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(fmt)?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioError::Format("no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(fmt)?;
    let mut samples: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    let mut channels = 0u16;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => {
                if samples.is_empty() {
                    return Err(fmt(e));
                }
                break; // a damaged tail: keep what was decoded
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(Error::DecodeError(_)) => continue, // one bad packet is skipped
            Err(e) => return Err(fmt(e)),
        };
        let spec = *buf.spec();
        let ch = u16::try_from(spec.channels.count()).unwrap_or(0);
        if rate == 0 {
            (rate, channels) = (spec.rate, ch);
        }
        if spec.rate != rate || ch != channels {
            return Err(AudioError::Format("the stream changes its format".into()));
        }
        let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        sb.copy_interleaved_ref(buf);
        if samples.len().saturating_add(sb.samples().len()) > MAX_DECODED_SAMPLES {
            return Err(AudioError::TooLong);
        }
        samples.extend_from_slice(sb.samples());
    }
    if rate == 0 || channels == 0 || !(8_000..=384_000).contains(&rate) || channels > 8 {
        return Err(AudioError::Format("no playable audio".into()));
    }
    Ok(Pcm {
        rate,
        channels,
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

    /// A 16-bit PCM WAV file.
    pub(crate) fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        v.extend_from_slice(&(channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    #[test]
    fn wav_and_opus_files_decode_with_their_length() {
        let pcm = decode(&wav(44_100, 2, &vec![1000i16; 44_100 * 2 * 3])).unwrap();
        assert_eq!((pcm.rate, pcm.channels, pcm.duration_ms()), (44_100, 2, 3000));
        let mut e = crate::opusogg::StreamEncoder::new(3).unwrap();
        e.push(&crate::tone::samples(0, 48_000 * 2)).unwrap();
        let (bytes, _) = e.finish().unwrap();
        assert_eq!(decode(&bytes).unwrap().duration_ms(), 2000);
    }

    #[test]
    fn hostile_files_are_errors() {
        let good = wav(8_000, 1, &[0i16; 8_000]);
        for junk in [
            &b""[..],
            b"RIFF",
            b"ID3\x04\0\0\0\0\0\0garbage",
            b"fLaC\0\0\0\x22",
            &good[..40],
            &[0xffu8; 2048][..],
        ] {
            assert!(decode(junk).is_err(), "{:?}", &junk[..junk.len().min(8)]);
        }
        // Every truncation of a valid file: an error or audio, never a panic.
        for cut in (0..good.len()).step_by(331) {
            let _ = decode(&good[..cut]);
        }
    }
}
