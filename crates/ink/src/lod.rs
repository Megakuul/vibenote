//! Levels of detail for drawing zoomed out (ADR 0003, M3.2; design `plan/design-m3-rendering.md`
//! §4.1). One Douglas–Peucker pass gives every point an **importance**: the split distance at
//! which it would be kept, clamped to its parent's so levels nest. Level `L` keeps the endpoints
//! and every point with importance above `tolerance(L) = 0.5 · 2^L` canvas units, so every
//! dropped point lies within that tolerance of the chord between its kept neighbours. The
//! distance includes the radius difference, so stroke widths do not jump.
//!
//! Bounded for hostile input: no recursion (explicit stack), and polylines are processed in
//! spans of at most [`SPAN`] points whose ends are always kept.

/// The kept levels besides L0 (L1/L2 remove too few points, ADR 0003 decision 4).
pub const LOD_LEVELS: [u8; 4] = [3, 4, 5, 6];
/// The smallest tolerance of any kept level (importances below it never matter).
const FINEST: f64 = 4.0; // tolerance(LOD_LEVELS[0])

/// Longest polyline span simplified as one (bounds the worst case at SPAN² per span).
pub const SPAN: usize = 1024;

/// Kept-point tolerance of a level, canvas units.
pub fn tolerance(level: u8) -> f64 {
    0.5 * f64::from(1u32 << level.min(30))
}

/// Distance from `p` to segment a–b in (x, y), plus the radius difference to the interpolated
/// radius at the closest point.
fn deviation(a: [f64; 3], b: [f64; 3], p: [f64; 3]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (a[0] + dx * t, a[1] + dy * t);
    let r = a[2] + (b[2] - a[2]) * t;
    ((p[0] - qx).hypot(p[1] - qy) + (p[2] - r).abs()).max(0.0)
}

/// Importance per point (`points[i] = [x, y, radius]`, canvas units). Endpoints (and span ends)
/// get `f32::INFINITY`. Non-finite input points are treated as always kept.
#[allow(clippy::cast_possible_truncation)] // importance only decides keep/drop; f32 is plenty
pub fn importance(points: &[[f64; 3]], out: &mut Vec<f32>) {
    out.clear();
    out.resize(points.len(), 0.0);
    let n = points.len();
    if n == 0 {
        return;
    }
    let mut stack: Vec<(usize, usize, f32)> = Vec::new();
    let mut start = 0usize;
    while start < n.saturating_sub(1) || start == 0 {
        let end = start.saturating_add(SPAN).min(n.saturating_sub(1));
        if let Some(v) = out.get_mut(start) {
            *v = f32::INFINITY;
        }
        if let Some(v) = out.get_mut(end) {
            *v = f32::INFINITY;
        }
        stack.push((start, end, f32::INFINITY));
        while let Some((a, b, parent)) = stack.pop() {
            if b <= a.saturating_add(1) {
                continue;
            }
            let (Some(&pa), Some(&pb)) = (points.get(a), points.get(b)) else {
                continue;
            };
            let mut best = (a.saturating_add(1), -1.0f64);
            for k in a.saturating_add(1)..b {
                let Some(&p) = points.get(k) else { continue };
                let d = if p.iter().all(|v| v.is_finite()) {
                    deviation(pa, pb, p)
                } else {
                    f64::INFINITY
                };
                if d > best.1 || d.is_nan() {
                    best = (k, if d.is_nan() { f64::INFINITY } else { d });
                }
            }
            let imp = (best.1 as f32).min(parent);
            if best.1 <= FINEST {
                // No interior point can pass any level's tolerance: they all get this span's
                // largest deviation (exact for every level) and the span is done. Without this,
                // straight runs make the search quadratic.
                for v in out.iter_mut().take(b).skip(a.saturating_add(1)) {
                    *v = imp;
                }
                continue;
            }
            if let Some(v) = out.get_mut(best.0) {
                *v = imp;
            }
            stack.push((a, best.0, imp));
            stack.push((best.0, b, imp));
        }
        if end >= n.saturating_sub(1) {
            break;
        }
        start = end;
    }
}

/// Indices kept at `level` (ascending; endpoints always kept), appended to `out`.
pub fn kept(importance: &[f32], level: u8, out: &mut Vec<u32>) {
    let tol = tolerance(level);
    for (i, &v) in importance.iter().enumerate() {
        if f64::from(v) > tol
            && let Ok(i) = u32::try_from(i)
        {
            out.push(i);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_line() -> impl Strategy<Value = Vec<[f64; 3]>> {
        prop::collection::vec((-30.0f64..30.0, -30.0f64..30.0, 0.0f64..3.0), 1..400).prop_map(|steps| {
            let (mut x, mut y) = (0.0, 0.0);
            steps
                .into_iter()
                .map(|(dx, dy, r)| {
                    x += dx;
                    y += dy;
                    [x, y, r]
                })
                .collect()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]
        /// Every dropped point lies within the level's tolerance of the chord between its kept
        /// neighbours; levels nest; endpoints are kept; the result is deterministic.
        #[test]
        fn dropped_points_stay_within_tolerance(pts in arb_line()) {
            let mut imp = Vec::new();
            importance(&pts, &mut imp);
            let mut again = Vec::new();
            importance(&pts, &mut again);
            prop_assert_eq!(&imp, &again);
            let mut prev: Option<Vec<u32>> = None;
            for level in LOD_LEVELS {
                let mut k = Vec::new();
                kept(&imp, level, &mut k);
                prop_assert_eq!(k.first().copied(), Some(0));
                prop_assert_eq!(k.last().copied(), Some((pts.len() - 1) as u32));
                for w in k.windows(2) {
                    let (a, b) = (w[0] as usize, w[1] as usize);
                    for p in &pts[a + 1..b] {
                        let d = deviation(pts[a], pts[b], *p);
                        prop_assert!(d <= tolerance(level) + 1e-6, "level {}: {} > {}", level, d, tolerance(level));
                    }
                }
                if let Some(p) = &prev {
                    prop_assert!(k.iter().all(|i| p.contains(i)), "level {} not nested", level);
                }
                prev = Some(k);
            }
        }
    }

    #[test]
    fn finest_matches_the_first_level() {
        assert!((FINEST - tolerance(LOD_LEVELS[0])).abs() < f64::EPSILON);
    }

    #[test]
    fn straight_lines_reduce_to_their_ends() {
        let pts: Vec<[f64; 3]> = (0..100).map(|i| [f64::from(i), 2.0 * f64::from(i), 1.0]).collect();
        let mut imp = Vec::new();
        importance(&pts, &mut imp);
        let mut k = Vec::new();
        kept(&imp, 3, &mut k);
        assert_eq!(k, vec![0, 99]);
    }

    #[test]
    fn hostile_input_is_bounded() {
        let n = 50_000;
        // a zig-zag where every point matters, plus a non-finite point and duplicates
        let mut pts: Vec<[f64; 3]> = (0..n)
            .map(|i| [f64::from(i), if i % 2 == 0 { 0.0 } else { 100.0 }, 1.0])
            .collect();
        pts[10] = [f64::NAN, 0.0, 1.0];
        pts.extend(std::iter::repeat_n([5.0, 5.0, 1.0], 3000));
        let t = std::time::Instant::now();
        let mut imp = Vec::new();
        importance(&pts, &mut imp);
        assert_eq!(imp.len(), pts.len());
        assert!(t.elapsed().as_secs() < 20);
        let mut k = Vec::new();
        kept(&imp, 6, &mut k);
        assert!(k.contains(&10));
        assert!(importance_of_empty_is_empty());
        // a million collinear points: linear time, not quadratic
        let line: Vec<[f64; 3]> = (0..1_000_000).map(|i| [f64::from(i) * 0.5, 7.0, 1.0]).collect();
        let t = std::time::Instant::now();
        importance(&line, &mut imp);
        assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
    }

    fn importance_of_empty_is_empty() -> bool {
        let mut imp = vec![1.0];
        importance(&[], &mut imp);
        imp.is_empty()
    }
}
