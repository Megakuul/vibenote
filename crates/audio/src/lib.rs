//! Audio (ADR 0009): recordings as one Ogg Opus stream split into chunks, decoding for
//! playback, import of common formats, a generated test source, and (feature `device`) capture
//! and playback through PipeWire. Nothing here runs on the input or render thread; callers use
//! the background workers in the engine.

pub mod import;
pub mod opusogg;
pub mod tone;

#[cfg(feature = "device")]
pub mod device;

/// Recording format: 48 kHz mono, 20 ms Opus frames.
pub const RATE: u32 = 48_000;
pub const FRAME: usize = 960;
/// A chunk (one blob) ends at the first page boundary after this much audio.
pub const CHUNK_MS: u64 = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("opus: {0}")]
    Opus(String),
    #[error("ogg: {0}")]
    Ogg(String),
    #[error("unsupported or damaged audio: {0}")]
    Format(String),
    #[error("audio too long")]
    TooLong,
    #[error("audio device: {0}")]
    Device(String),
}

/// Decoded audio: interleaved samples.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Pcm {
    /// Length in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        let frames = self.samples.len().checked_div(usize::from(self.channels)).unwrap_or(0);
        let frames = u64::try_from(frames).unwrap_or(u64::MAX);
        frames
            .saturating_mul(1000)
            .checked_div(u64::from(self.rate))
            .unwrap_or(0)
    }
}

/// Most audio decoded at once (about 3 hours of 48 kHz stereo): longer files are refused.
pub const MAX_DECODED_SAMPLES: usize = 48_000 * 2 * 3 * 3600;
