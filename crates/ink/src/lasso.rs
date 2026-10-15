//! Lasso geometry (R-PEN-009): how much of a stroke lies inside a lasso polygon. A stroke is
//! selected when at least half of its length is inside [SPEC proposal]. Each segment is cut where
//! it crosses the polygon's edges; each piece is inside or outside as a whole (even-odd rule).
//!
//! Bounded for hostile input: polygons are capped at [`MAX_LASSO_POINTS`]; segments outside the
//! polygon's box are skipped without testing edges.

/// Longest lasso path accepted (points).
pub const MAX_LASSO_POINTS: usize = 4096;

/// Rejects hostile lassos (too short/long, non-finite).
pub fn check(polygon: &[(f64, f64)]) -> Result<(), String> {
    if polygon.len() < 3 || polygon.len() > MAX_LASSO_POINTS {
        return Err(format!("a lasso needs 3..={MAX_LASSO_POINTS} points"));
    }
    let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
    if polygon.iter().all(|&(x, y)| ok(x) && ok(y)) {
        Ok(())
    } else {
        Err("lasso path out of range".into())
    }
}

fn edges(polygon: &[(f64, f64)]) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
    let n = polygon.len();
    (0..n).filter_map(move |i| Some((*polygon.get(i)?, *polygon.get(i.saturating_add(1).checked_rem(n)?)?)))
}

/// Even-odd point-in-polygon test (the polygon is closed implicitly).
pub fn contains(polygon: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut inside = false;
    for (a, b) in edges(polygon) {
        if (a.1 > p.1) != (b.1 > p.1) {
            let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
            if p.0 < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Parameter on a→b where it crosses c→d, if it does.
fn cross_at(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> Option<f64> {
    let r = (b.0 - a.0, b.1 - a.1);
    let s = (d.0 - c.0, d.1 - c.1);
    let den = r.0 * s.1 - r.1 * s.0;
    if den.abs() < 1e-300 {
        return None;
    }
    let t = ((c.0 - a.0) * s.1 - (c.1 - a.1) * s.0) / den;
    let u = ((c.0 - a.0) * r.1 - (c.1 - a.1) * r.0) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// Share (0..=1) of the polyline's length inside the polygon. A single point (or a polyline
/// of zero length) counts as fully inside or outside.
pub fn inside_fraction(points: &[(f64, f64)], polygon: &[(f64, f64)]) -> f64 {
    let Some(&first) = points.first() else { return 0.0 };
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in polygon {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let (mut total, mut inside) = (0.0f64, 0.0f64);
    let mut ts: Vec<f64> = Vec::new();
    for w in points.windows(2) {
        let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else {
            continue;
        };
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        total += len;
        if len == 0.0 || a.0.max(b.0) < x0 || a.0.min(b.0) > x1 || a.1.max(b.1) < y0 || a.1.min(b.1) > y1 {
            continue;
        }
        ts.clear();
        ts.push(0.0);
        ts.extend(edges(polygon).filter_map(|(c, d)| cross_at(a, b, c, d)));
        ts.push(1.0);
        ts.sort_by(f64::total_cmp);
        for t in ts.windows(2) {
            let (Some(&t0), Some(&t1)) = (t.first(), t.get(1)) else {
                continue;
            };
            if t1 <= t0 {
                continue;
            }
            let m = f64::midpoint(t0, t1);
            if contains(polygon, (a.0 + (b.0 - a.0) * m, a.1 + (b.1 - a.1) * m)) {
                inside += (t1 - t0) * len;
            }
        }
    }
    if total == 0.0 {
        return if contains(polygon, first) { 1.0 } else { 0.0 };
    }
    (inside / total).clamp(0.0, 1.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SQUARE: [(f64, f64); 4] = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];

    #[test]
    fn fractions_of_simple_lines() {
        assert!((inside_fraction(&[(10.0, 10.0), (40.0, 10.0)], &SQUARE) - 1.0).abs() < 1e-12);
        assert!(inside_fraction(&[(200.0, 200.0), (300.0, 200.0)], &SQUARE).abs() < 1e-12);
        // half in, half out
        assert!((inside_fraction(&[(50.0, 50.0), (150.0, 50.0)], &SQUARE) - 0.5).abs() < 1e-12);
        // a dot
        assert_eq!(inside_fraction(&[(5.0, 5.0)], &SQUARE), 1.0);
        assert_eq!(inside_fraction(&[(500.0, 5.0)], &SQUARE), 0.0);
    }

    #[test]
    fn hostile_lassos_are_rejected() {
        assert!(check(&[(0.0, 0.0), (1.0, 1.0)]).is_err());
        assert!(check(&[(0.0, 0.0), (1.0, f64::NAN), (2.0, 0.0)]).is_err());
        assert!(check(&vec![(0.0, 0.0); MAX_LASSO_POINTS + 1]).is_err());
    }

    proptest! {
        /// Shares stay within 0..=1, and growing the lasso never shrinks a stroke's share.
        #[test]
        fn fractions_are_bounded_and_monotonic_under_growing_lassos(
            pts in prop::collection::vec((-50.0f64..150.0, -50.0f64..150.0), 1..30),
            grow in 0.0f64..200.0,
        ) {
            let small = inside_fraction(&pts, &SQUARE);
            let big: Vec<(f64, f64)> = SQUARE.iter().map(|&(x, y)| (if x > 50.0 { x + grow } else { x - grow }, if y > 50.0 { y + grow } else { y - grow })).collect();
            let large = inside_fraction(&pts, &big);
            prop_assert!((0.0..=1.0).contains(&small) && (0.0..=1.0).contains(&large));
            prop_assert!(large + 1e-9 >= small, "{} < {}", large, small);
        }
    }
}
