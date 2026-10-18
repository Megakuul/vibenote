//! Wall-clock helpers: Unix milliseconds, UTC days and weekdays, SigV4 timestamps.

use std::time::{SystemTime, UNIX_EPOCH};

const DAY_MS: u64 = 86_400_000;
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Milliseconds since the Unix epoch (0 before it).
pub fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The UTC weekday of `ms` (`mon` … `sun`; 1970-01-01 was a Thursday).
pub fn weekday(ms: u64) -> &'static str {
    let days = ms / DAY_MS;
    let i = usize::try_from(days.wrapping_add(3) % 7).unwrap_or(0);
    WEEKDAYS.get(i).copied().unwrap_or("mon")
}

/// True for the weekday names used by [`weekday`].
pub fn is_weekday(s: &str) -> bool {
    WEEKDAYS.contains(&s)
}

/// UTC civil date (year, month, day) of `ms` (H. Hinnant's `civil_from_days`).
pub fn civil(ms: u64) -> (u64, u64, u64) {
    let z = (ms / DAY_MS).wrapping_add(719_468);
    let era = z / 146_097;
    let doe = z.wrapping_sub(era.wrapping_mul(146_097));
    let yoe = doe
        .wrapping_sub(doe / 1460)
        .wrapping_add(doe / 36_524)
        .wrapping_sub(doe / 146_096)
        / 365;
    let doy = doe.wrapping_sub(yoe.wrapping_mul(365).wrapping_add(yoe / 4).wrapping_sub(yoe / 100));
    let mp = doy.wrapping_mul(5).wrapping_add(2) / 153;
    let d = doy
        .wrapping_sub(mp.wrapping_mul(153).wrapping_add(2) / 5)
        .wrapping_add(1);
    let m = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    let y = yoe.wrapping_add(era.wrapping_mul(400)).wrapping_add(u64::from(m <= 2));
    (y, m, d)
}

/// `YYYY-MM-DD` (UTC).
pub fn date(ms: u64) -> String {
    let (y, m, d) = civil(ms);
    format!("{y:04}-{m:02}-{d:02}")
}

/// SigV4 timestamp `YYYYMMDD'T'HHMMSS'Z'` (UTC).
pub fn amz_date(ms: u64) -> String {
    let (y, m, d) = civil(ms);
    let secs = (ms % DAY_MS) / 1000;
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_weekdays() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(weekday(0), "thu");
        // 2026-10-18 is a Sunday; 2000-02-29 a leap day.
        let ms = 1_792_281_600_000; // 2026-10-18T00:00:00Z
        assert_eq!(date(ms), "2026-10-18");
        assert_eq!(weekday(ms), "sun");
        assert_eq!(date(951_782_400_000), "2000-02-29");
        assert_eq!(amz_date(1_440_938_160_000), "20150830T123600Z");
    }
}
