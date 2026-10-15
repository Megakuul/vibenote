//! A generated test source (R-AUDIO tests never use the microphone): a 440 Hz tone with a
//! short click every second, deterministic from the sample index.

use crate::RATE;

/// Samples `start..start + n` of the test tone.
pub fn samples(start: u64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|k| {
            let i = start.saturating_add(u64::try_from(k).unwrap_or(u64::MAX));
            #[allow(clippy::cast_precision_loss)] // sample index → seconds (exact enough)
            let t = i.checked_rem(u64::from(RATE).saturating_mul(3600)).unwrap_or(0) as f64 / f64::from(RATE);
            let click = if i.checked_rem(u64::from(RATE)).unwrap_or(0) < 240 {
                0.3
            } else {
                0.0
            };
            #[allow(clippy::cast_possible_truncation)] // amplitude ≤ 0.6
            let v = (0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() + click) as f32;
            v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn deterministic_and_bounded() {
        let a = super::samples(1000, 480);
        assert_eq!(a, super::samples(1000, 480));
        assert!(a.iter().all(|v| v.abs() <= 0.61));
    }
}
