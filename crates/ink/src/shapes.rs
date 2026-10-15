//! Snap from ink (R-SHAPE-002): recognise a rough hand-drawn line, arrow, triangle, rectangle
//! or ellipse. Conservative: anything unclear is not a shape (the ink stays as it is).
//!
//! The path is simplified (Douglas–Peucker with a tolerance relative to its size); the
//! remaining corners decide the class: an open path with no inner corner is a line; shaft,
//! tip, barb, back to the tip, barb is an arrow; a closed path with 3 corners is a triangle,
//! 4 roughly right-angled corners a rectangle; a closed path that stays close to the ellipse
//! of its bounding box is an ellipse.

use vibenote_geom::Point;
use vibenote_model::ShapeKind;

/// Points of a recognised shape, in `vibenote_model::shape_outline`'s convention.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapped {
    pub kind: ShapeKind,
    pub points: Vec<Point>,
    pub closed: bool,
}

fn dist(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn seg_dist(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, Point::new(a.x + dx * t, a.y + dy * t))
}

/// Douglas–Peucker; keeps the first and last point.
fn simplify(p: &[Point], tol: f64) -> Vec<Point> {
    if p.len() < 3 {
        return p.to_vec();
    }
    let mut keep = vec![false; p.len()];
    if let Some(k) = keep.first_mut() {
        *k = true;
    }
    if let Some(k) = keep.last_mut() {
        *k = true;
    }
    let mut stack = vec![(0usize, p.len().saturating_sub(1))];
    while let Some((a, b)) = stack.pop() {
        let (Some(&pa), Some(&pb)) = (p.get(a), p.get(b)) else {
            continue;
        };
        let mut worst = (0.0f64, a);
        for i in a.saturating_add(1)..b {
            if let Some(&q) = p.get(i) {
                let d = seg_dist(q, pa, pb);
                if d > worst.0 {
                    worst = (d, i);
                }
            }
        }
        if worst.0 > tol {
            if let Some(k) = keep.get_mut(worst.1) {
                *k = true;
            }
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    p.iter().zip(keep).filter(|(_, k)| *k).map(|(q, _)| *q).collect()
}

fn angle_at(a: Point, b: Point, c: Point) -> f64 {
    let (u, v) = ((a.x - b.x, a.y - b.y), (c.x - b.x, c.y - b.y));
    let d = u.0.hypot(u.1) * v.0.hypot(v.1);
    if d == 0.0 {
        return 0.0;
    }
    ((u.0 * v.0 + u.1 * v.1) / d).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Recognises a shape in a stroke's points, or `None`.
pub fn classify(points: &[Point]) -> Option<Snapped> {
    if points.len() < 2 || points.len() > 100_000 || !points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in points {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    let diag = (x1 - x0).hypot(y1 - y0);
    if diag < 4.0 {
        return None; // a dot
    }
    let length: f64 = points
        .windows(2)
        .filter_map(|w| Some(dist(*w.first()?, *w.get(1)?)))
        .sum();
    let (first, last) = (*points.first()?, *points.last()?);
    let closed = dist(first, last) < 0.12 * length.max(1e-9);
    if !closed {
        let s = simplify(points, 0.05 * diag);
        if s.len() == 2 {
            return Some(Snapped {
                kind: ShapeKind::Line,
                points: vec![first, last],
                closed: false,
            });
        }
        // Arrow drawn in one stroke: tail → tip → barb → tip → barb.
        if s.len() == 5 {
            let (t, tip, b1, back, b2) = (s.first()?, s.get(1)?, s.get(2)?, s.get(3)?, s.get(4)?);
            let shaft = dist(*t, *tip);
            let barbs_short = dist(*tip, *b1) < 0.5 * shaft && dist(*tip, *b2) < 0.5 * shaft;
            let returns = dist(*back, *tip) < 0.25 * shaft;
            let a1 = angle_at(*t, *tip, *b1);
            let a2 = angle_at(*t, *tip, *b2);
            if barbs_short && returns && (10.0..70.0).contains(&a1) && (10.0..70.0).contains(&a2) {
                return Some(Snapped {
                    kind: ShapeKind::Arrow,
                    points: vec![*t, *tip],
                    closed: false,
                });
            }
        }
        return None;
    }
    // Closed: corners of the simplified loop (the closing point dropped).
    let mut s = simplify(points, 0.08 * diag);
    if s.len() > 2 && s.first().zip(s.last()).is_some_and(|(a, b)| dist(*a, *b) < 0.15 * diag) {
        s.pop();
    }
    // Merge a start point lying on an edge (the stroke began mid-side).
    if s.len() >= 4
        && let (Some(&prev), Some(&cur), Some(&next)) = (s.last(), s.first(), s.get(1))
        && angle_at(prev, cur, next) > 160.0
    {
        s.remove(0);
    }
    let corners_ok = |want: f64, tol: f64| {
        let n = s.len();
        (0..n).all(|i| {
            let (Some(&a), Some(&b), Some(&c)) = (
                s.get(i.checked_sub(1).unwrap_or(n.saturating_sub(1))),
                s.get(i),
                s.get(i.saturating_add(1).checked_rem(n).unwrap_or(0)),
            ) else {
                return false;
            };
            (angle_at(a, b, c) - want).abs() <= tol
        })
    };
    if s.len() == 3 {
        return Some(Snapped {
            kind: ShapeKind::Triangle,
            points: s,
            closed: true,
        });
    }
    if s.len() == 4 && corners_ok(90.0, 25.0) {
        // Axis-aligned when the first side is within 15° of an axis: a clean box.
        let side = (s.get(1)?.y - s.first()?.y)
            .atan2(s.get(1)?.x - s.first()?.x)
            .to_degrees()
            .rem_euclid(90.0);
        if !(15.0..=75.0).contains(&side) {
            return Some(Snapped {
                kind: ShapeKind::Rectangle,
                points: vec![Point::new(x0, y0), Point::new(x1, y1)],
                closed: true,
            });
        }
        return Some(Snapped {
            kind: ShapeKind::Rectangle,
            points: s,
            closed: true,
        });
    }
    // Ellipse: every point near the bounding-box ellipse.
    let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
    if rx > 1.0 && ry > 1.0 {
        let worst = points
            .iter()
            .map(|p| (((p.x - cx) / rx).hypot((p.y - cy) / ry) - 1.0).abs())
            .fold(0.0f64, f64::max);
        if worst < 0.2 {
            return Some(Snapped {
                kind: ShapeKind::Ellipse,
                points: vec![Point::new(x0, y0), Point::new(x1, y1)],
                closed: true,
            });
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::cast_precision_loss)]
mod tests {
    use super::*;

    fn wobble(p: &[(f64, f64)], per_side: usize, amp: f64, seed: u64) -> Vec<Point> {
        let mut out = Vec::new();
        let mut r = seed;
        let mut noise = || {
            r = r
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((r >> 33) as f64 / f64::from(u32::MAX) - 0.25) * amp
        };
        for w in p.windows(2) {
            for k in 0..per_side {
                let t = k as f64 / per_side as f64;
                out.push(Point::new(
                    w[0].0 + (w[1].0 - w[0].0) * t + noise(),
                    w[0].1 + (w[1].1 - w[0].1) * t + noise(),
                ));
            }
        }
        let l = p.last().unwrap();
        out.push(Point::new(l.0, l.1));
        out
    }

    /// A generated set of rough shapes (seeded jitter): each is recognised as its class.
    #[test]
    fn rough_shapes_are_recognised() {
        let mut total = 0;
        let mut right = 0;
        for seed in 0..40u64 {
            let amp = 2.0 + (seed % 5) as f64;
            let cases: Vec<(ShapeKind, Vec<Point>)> = vec![
                (ShapeKind::Line, wobble(&[(0.0, 0.0), (180.0, 30.0)], 30, amp, seed)),
                (
                    ShapeKind::Triangle,
                    wobble(&[(0.0, 0.0), (160.0, 10.0), (60.0, 140.0), (0.0, 0.0)], 20, amp, seed),
                ),
                (
                    ShapeKind::Rectangle,
                    wobble(
                        &[(0.0, 0.0), (200.0, 0.0), (200.0, 120.0), (0.0, 120.0), (0.0, 0.0)],
                        20,
                        amp,
                        seed,
                    ),
                ),
                (
                    ShapeKind::Ellipse,
                    (0..=80)
                        .map(|k| {
                            let t = k as f64 / 80.0 * std::f64::consts::TAU;
                            Point::new(
                                100.0 + 90.0 * t.cos() + amp * (t * 7.0 + seed as f64).sin(),
                                60.0 + 50.0 * t.sin(),
                            )
                        })
                        .collect(),
                ),
                (
                    ShapeKind::Arrow,
                    wobble(
                        &[
                            (0.0, 100.0),
                            (200.0, 100.0),
                            (170.0, 82.0),
                            (200.0, 100.0),
                            (170.0, 118.0),
                        ],
                        20,
                        amp * 0.6,
                        seed,
                    ),
                ),
            ];
            for (want, pts) in cases {
                total += 1;
                if classify(&pts).map(|s| s.kind) == Some(want) {
                    right += 1;
                }
            }
        }
        assert!(f64::from(right) >= 0.95 * f64::from(total), "{right}/{total}");
    }

    #[test]
    fn scribbles_and_writing_are_not_shapes() {
        let zigzag: Vec<Point> = (0..60)
            .map(|k| Point::new(f64::from(k) * 5.0, if k % 2 == 0 { 0.0 } else { 40.0 }))
            .collect();
        assert_eq!(classify(&zigzag), None);
        let loops: Vec<Point> = (0..200)
            .map(|k| {
                let t = f64::from(k) / 20.0;
                Point::new(t * 30.0 + 15.0 * t.cos(), 15.0 * t.sin())
            })
            .collect();
        assert_eq!(classify(&loops), None);
        assert_eq!(classify(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)]), None, "a dot");
    }
}
