//! simtab library: stroke file format (v1), deterministic stroke generator, conversion to ink
//! fixtures. The uinput device player lives in the binary (`device.rs`).
//!
//! Reviewed when graduating from `spikes/input/simtab` (Phase 3): strict lints, no `unsafe`,
//! no panicking constructs, bounded inputs.

pub mod fixture;
pub mod generate;
pub mod stroke;

/// CLOCK_MONOTONIC in nanoseconds (same clock as libinput and wp_presentation).
pub fn mono_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(t.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(t.tv_nsec).unwrap_or(0))
}

/// Checked float → u64 (non-finite or negative → 0, saturating above u64::MAX).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // range checked above the cast
pub fn f64_to_u64(x: f64) -> u64 {
    if !x.is_finite() || x <= 0.0 {
        0
    } else if x >= 1.8e19 {
        u64::MAX
    } else {
        x as u64
    }
}

/// Checked float → i32 (rounded, clamped to the given range; non-finite → `lo`).
#[allow(clippy::cast_possible_truncation)] // clamped into i32 range before the cast
pub fn f64_to_i32(x: f64, lo: i32, hi: i32) -> i32 {
    if !x.is_finite() {
        return lo;
    }
    x.round().clamp(f64::from(lo), f64::from(hi)) as i32
}
