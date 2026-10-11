//! Shared spike code: monotonic clock, event/frame logging, ink mesh, prediction, wgpu renderer.
//! Throwaway (Phase 0). Not product code.

pub mod gpu;
pub mod ink;
pub mod log;

/// CLOCK_MONOTONIC in ns (same clock as simtab, libinput and wp_presentation on this machine).
pub fn mono_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Read a boolean/number option from the environment (spike configuration knobs).
pub fn env_or<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
