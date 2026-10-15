//! Writing blocks (R-HWR-001): strokes in time order form a block until the pen rests longer
//! than the idle threshold (0.5–10 s, default 1.5 s) or the next stroke starts far away from
//! the block (a jump to another place on the page).

/// A stroke as segmentation sees it: start/end time (µs) and its points.
#[derive(Clone, Debug)]
pub struct TimedStroke {
    pub start_us: u64,
    pub end_us: u64,
    pub points: Vec<(f64, f64)>,
}

/// A jump this many times the block's typical stroke height (and at least `MIN_JUMP` units)
/// starts a new block.
const JUMP_FACTOR: f64 = 6.0;
const MIN_JUMP: f64 = 120.0;

fn bbox(p: &[(f64, f64)]) -> Option<(f64, f64, f64, f64)> {
    let mut it = p.iter().filter(|q| q.0.is_finite() && q.1.is_finite());
    let f = it.next()?;
    Some(it.fold((f.0, f.1, f.0, f.1), |(x0, y0, x1, y1), q| {
        (x0.min(q.0), y0.min(q.1), x1.max(q.0), y1.max(q.1))
    }))
}

fn gap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let dx = (a.0 - b.2).max(b.0 - a.2).max(0.0);
    let dy = (a.1 - b.3).max(b.1 - a.3).max(0.0);
    dx.hypot(dy)
}

/// Blocks as inclusive stroke index ranges `[first, last]`, in order.
pub fn segment(strokes: &[TimedStroke], idle_ms: u32) -> Vec<[usize; 2]> {
    let idle = u64::from(idle_ms).saturating_mul(1000);
    let mut out = Vec::new();
    let mut first = 0usize;
    let mut last_end = 0u64;
    let mut block: Option<(f64, f64, f64, f64)> = None;
    let mut heights: Vec<f64> = Vec::new();
    for (k, s) in strokes.iter().enumerate() {
        let b = bbox(&s.points);
        let new_block = k > 0
            && (s.start_us.saturating_sub(last_end) > idle
                || match (block, b) {
                    (Some(blk), Some(sb)) => {
                        let mut hs = heights.clone();
                        hs.sort_by(f64::total_cmp);
                        let typical = hs.get(hs.len() / 2).copied().unwrap_or(0.0);
                        gap(blk, sb) > (JUMP_FACTOR * typical).max(MIN_JUMP)
                    }
                    _ => false,
                });
        if new_block {
            out.push([first, k.saturating_sub(1)]);
            first = k;
            block = None;
            heights.clear();
        }
        if let Some(sb) = b {
            block = Some(match block {
                Some(o) => (o.0.min(sb.0), o.1.min(sb.1), o.2.max(sb.2), o.3.max(sb.3)),
                None => sb,
            });
            heights.push(sb.3 - sb.1);
        }
        last_end = s.end_us.max(s.start_us);
    }
    if !strokes.is_empty() {
        out.push([first, strokes.len().saturating_sub(1)]);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn s(t: u64, x: f64) -> TimedStroke {
        TimedStroke {
            start_us: t,
            end_us: t + 100_000,
            points: vec![(x, 0.0), (x + 10.0, 20.0)],
        }
    }

    #[test]
    fn idle_gaps_and_jumps_split_blocks() {
        let strokes = [s(0, 0.0), s(300_000, 15.0), s(2_500_000, 30.0), s(2_700_000, 2000.0)];
        assert_eq!(segment(&strokes, 1500), vec![[0, 1], [2, 2], [3, 3]]);
        assert_eq!(segment(&strokes, 10_000), vec![[0, 2], [3, 3]]);
        assert!(segment(&[], 1500).is_empty());
    }
}
