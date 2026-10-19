//! Snap from ink (R-SHAPE-002): recognise a rough hand-drawn line, arrow, triangle, rectangle
//! or ellipse. Conservative: anything unclear is not a shape (the ink stays as it is).
//!
//! An open path that stays close to its best-fit line (small hooks at the ends and a slight bow
//! allowed) is a line, levelled exactly when it is within a few degrees of horizontal or
//! vertical. Otherwise the path is simplified (Douglas–Peucker with a tolerance relative to its
//! size) and the remaining corners decide the class: shaft,
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

/// Share of the path length at each end left out when judging straightness (hooks where the
/// pen lands and lifts).
const HOOK_SHARE: f64 = 0.08;
/// Most distance of the path from its best-fit line, relative to the line's length.
const LINE_DEVIATION: f64 = 0.06;
/// A line within this angle (degrees) of horizontal or vertical is levelled exactly.
const LEVEL_DEG: f64 = 6.0;

/// The points of a path between `from` and `to` (shares of its length).
fn between(points: &[Point], length: f64, from: f64, to: f64) -> Vec<Point> {
    let mut out = Vec::new();
    let mut walked = 0.0;
    let mut prev: Option<Point> = None;
    for p in points {
        if let Some(q) = prev {
            walked += dist(q, *p);
        }
        prev = Some(*p);
        let share = if length > 0.0 { walked / length } else { 0.0 };
        if (from..=to).contains(&share) {
            out.push(*p);
        }
    }
    out
}

/// An arrow drawn in one stroke: (tail, tip). The head may be drawn tip → barb → tip → barb,
/// tip → barb → barb (→ tip) (a triangle), and the whole arrow from either end.
fn arrow(points: &[Point], diag: f64) -> Option<(Point, Point)> {
    for tol in [0.05, 0.035, 0.07] {
        let s = simplify(points, tol * diag);
        let mut r = s.clone();
        r.reverse();
        for s in [s, r] {
            if let Some(found) = arrow_corners(&s) {
                return Some(found);
            }
        }
    }
    None
}

/// The arrow patterns on simplified corners starting at the tail.
fn arrow_corners(s: &[Point]) -> Option<(Point, Point)> {
    let (t, tip) = (*s.first()?, *s.get(1)?);
    let shaft = dist(t, tip);
    if shaft <= 0.0 {
        return None;
    }
    let barb_ok = |b: Point| {
        let a = angle_at(t, tip, b);
        dist(tip, b) < 0.6 * shaft && dist(tip, b) > 0.04 * shaft && (12.0..75.0).contains(&a)
    };
    // The two barbs lie on opposite sides of the shaft.
    let side = |b: Point| (tip.x - t.x) * (b.y - t.y) - (tip.y - t.y) * (b.x - t.x);
    let opposite = |a: Point, b: Point| side(a) * side(b) < 0.0;
    let near_tip = |p: Point| dist(p, tip) < 0.3 * shaft;
    let head = |b1: Point, b2: Point| barb_ok(b1) && barb_ok(b2) && opposite(b1, b2);
    let found = match s.len() {
        // tail → tip → barb → back to the tip → barb, or tail → tip → barb → barb → tip (a
        // closed triangle head)
        5 => {
            let (b1, x, y) = (*s.get(2)?, *s.get(3)?, *s.get(4)?);
            (near_tip(x) && head(b1, y)) || (near_tip(y) && head(b1, x))
        }
        // tail → tip → barb → barb (an open triangle head)
        4 => head(*s.get(2)?, *s.get(3)?),
        _ => false,
    };
    found.then_some((t, tip))
}

/// An arrow head drawn as its own stroke ("<", "^", ">"): (apex, unit vector from the apex into
/// the head's opening, arm length). `None` unless the path is two straight arms of similar
/// length meeting at 25°–120°.
pub fn caret(points: &[Point]) -> Option<(Point, (f64, f64), f64)> {
    if points.len() < 3 || !points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
        return None;
    }
    let (first, last) = (*points.first()?, *points.last()?);
    let mut far = (0.0f64, first);
    for p in points {
        let d = seg_dist(*p, first, last);
        if d > far.0 {
            far = (d, *p);
        }
    }
    let apex = far.1;
    let (a, b) = (dist(apex, first), dist(apex, last));
    if a <= 0.0 || b <= 0.0 || a.max(b) > 2.0 * a.min(b) {
        return None;
    }
    let s = simplify(points, 0.12 * a.max(b));
    if s.len() != 3 {
        return None;
    }
    let angle = angle_at(first, apex, last);
    if !(25.0..120.0).contains(&angle) {
        return None;
    }
    let mid = Point::new((first.x + last.x) / 2.0, (first.y + last.y) / 2.0);
    let (dx, dy) = (mid.x - apex.x, mid.y - apex.y);
    let n = dx.hypot(dy);
    if n <= 0.0 {
        return None;
    }
    Some((apex, (dx / n, dy / n), f64::midpoint(a, b)))
}

/// The straight line a path looks like: its best fit (principal axis), spanning the path's
/// extent along it, levelled near the axes. `None` if the path strays from it.
fn fit_line(points: &[Point], length: f64) -> Option<Snapped> {
    let mut core = between(points, length, HOOK_SHARE, 1.0 - HOOK_SHARE);
    // A sparse path (a few samples) has no hooks to leave out.
    if core.len() < 2 {
        core = points.to_vec();
    }
    #[allow(clippy::cast_precision_loss)] // a point count
    let n = points.len() as f64;
    let (mx, my) = (
        points.iter().map(|p| p.x).sum::<f64>() / n,
        points.iter().map(|p| p.y).sum::<f64>() / n,
    );
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in points {
        let (dx, dy) = (p.x - mx, p.y - my);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (ux, uy) = (theta.cos(), theta.sin());
    let along = |p: &Point| (p.x - mx) * ux + (p.y - my) * uy;
    let across = |p: &Point| (-(p.x - mx) * uy + (p.y - my) * ux).abs();
    let (t0, t1) = points
        .iter()
        .map(along)
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), t| (a.min(t), b.max(t)));
    let span = t1 - t0;
    if !(span.is_finite() && span > 0.0) {
        return None;
    }
    // The core stays near the line, and the path does not double back (a "V" or a zigzag).
    if core.iter().map(across).fold(0.0f64, f64::max) > LINE_DEVIATION * span || length > 1.25 * span {
        return None;
    }
    let (mut a, mut b) = (
        Point::new(mx + ux * t0, my + uy * t0),
        Point::new(mx + ux * t1, my + uy * t1),
    );
    // In drawing order.
    let (first, last) = (*points.first()?, *points.last()?);
    if dist(first, a) + dist(last, b) > dist(first, b) + dist(last, a) {
        std::mem::swap(&mut a, &mut b);
    }
    let deg = theta.to_degrees().rem_euclid(180.0);
    let (cx, cy) = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    if deg.min(180.0 - deg) <= LEVEL_DEG {
        a.y = cy;
        b.y = cy;
    } else if (deg - 90.0).abs() <= LEVEL_DEG {
        a.x = cx;
        b.x = cx;
    }
    Some(Snapped {
        kind: ShapeKind::Line,
        points: vec![a, b],
        closed: false,
    })
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
        if let Some(line) = fit_line(points, length) {
            return Some(line);
        }
        if let Some((tail, tip)) = arrow(points, diag) {
            return Some(Snapped {
                kind: ShapeKind::Arrow,
                points: vec![tail, tip],
                closed: false,
            });
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
            // Each side where its two corners are on average (overshoot at a corner does not
            // grow the box).
            let mut xs: Vec<f64> = s.iter().map(|p| p.x).collect();
            let mut ys: Vec<f64> = s.iter().map(|p| p.y).collect();
            xs.sort_by(f64::total_cmp);
            ys.sort_by(f64::total_cmp);
            let mean = |v: &[f64], i: usize| {
                (v.get(i).copied().unwrap_or(0.0) + v.get(i.saturating_add(1)).copied().unwrap_or(0.0)) / 2.0
            };
            return Some(Snapped {
                kind: ShapeKind::Rectangle,
                points: vec![
                    Point::new(mean(&xs, 0), mean(&ys, 0)),
                    Point::new(mean(&xs, 2), mean(&ys, 2)),
                ],
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
        let mut off: Vec<f64> = points
            .iter()
            .map(|p| (((p.x - cx) / rx).hypot((p.y - cy) / ry) - 1.0).abs())
            .collect();
        off.sort_by(f64::total_cmp);
        let worst = off.last().copied().unwrap_or(f64::INFINITY);
        // Most of the path near the ellipse; a small overshoot where it closes is allowed.
        let most = off.get(off.len().saturating_mul(9) / 10).copied().unwrap_or(worst);
        if most < 0.18 && worst < 0.35 {
            // Nearly round: a circle.
            let (rx, ry) = if (rx - ry).abs() < 0.1 * rx.max(ry) {
                let r = (rx + ry) / 2.0;
                (r, r)
            } else {
                (rx, ry)
            };
            return Some(Snapped {
                kind: ShapeKind::Ellipse,
                points: vec![Point::new(cx - rx, cy - ry), Point::new(cx + rx, cy + ry)],
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

    /// Arrows drawn in one stroke, in the ways people draw them (owner 2026-10-19: "add
    /// autocorrection for arrows"), from either end, also slanted; the tip is kept.
    #[test]
    fn one_stroke_arrows_in_many_ways() {
        let heads: [&[(f64, f64)]; 4] = [
            // tip → barb → tip → barb
            &[
                (0.0, 100.0),
                (200.0, 100.0),
                (170.0, 82.0),
                (200.0, 100.0),
                (170.0, 118.0),
            ],
            // tip → barb → barb → tip (closed triangle)
            &[
                (0.0, 100.0),
                (200.0, 100.0),
                (172.0, 84.0),
                (172.0, 116.0),
                (200.0, 100.0),
            ],
            // tip → barb → barb (open triangle)
            &[(0.0, 100.0), (200.0, 100.0), (172.0, 84.0), (172.0, 116.0)],
            // head first: barb → tip → barb → tip → tail
            &[
                (170.0, 82.0),
                (200.0, 100.0),
                (170.0, 118.0),
                (200.0, 100.0),
                (0.0, 100.0),
            ],
        ];
        let rot = |p: &[(f64, f64)], deg: f64| -> Vec<(f64, f64)> {
            let (sn, c) = deg.to_radians().sin_cos();
            p.iter().map(|&(x, y)| (x * c - y * sn, x * sn + y * c)).collect()
        };
        let (mut total, mut right) = (0, 0);
        for (h, head) in heads.iter().enumerate() {
            for deg in [0.0, 35.0, 120.0, 250.0] {
                for seed in 0..10u64 {
                    total += 1;
                    let pts = wobble(&rot(head, deg), 20, 1.5 + (seed % 3) as f64, seed);
                    let Some(sn) = classify(&pts) else {
                        eprintln!("head {h} at {deg}°, seed {seed}: none");
                        continue;
                    };
                    let tip = rot(&[(200.0, 100.0)], deg)[0];
                    if sn.kind == ShapeKind::Arrow && dist(sn.points[1], Point::new(tip.0, tip.1)) < 12.0 {
                        right += 1;
                    } else {
                        eprintln!("head {h} at {deg}°, seed {seed}: {:?}", sn.kind);
                    }
                }
            }
        }
        assert!(f64::from(right) >= 0.9 * f64::from(total), "{right}/{total}");
        // A plain zigzag is no arrow.
        let zig = wobble(&[(0.0, 0.0), (100.0, 40.0), (0.0, 80.0), (100.0, 120.0)], 20, 1.0, 1);
        assert_ne!(classify(&zig).map(|s| s.kind), Some(ShapeKind::Arrow));
    }

    /// A separate head (">"): its apex and the direction it opens to.
    #[test]
    fn carets_are_heads() {
        let gt = wobble(&[(170.0, 80.0), (200.0, 100.0), (170.0, 120.0)], 15, 1.5, 3);
        let (apex, dir, arm) = caret(&gt).expect("a head");
        assert!(dist(apex, Point::new(200.0, 100.0)) < 4.0, "{apex:?}");
        assert!(dir.0 < -0.9, "opens to the left: {dir:?}");
        assert!((arm - 36.0).abs() < 6.0, "{arm}");
        // A straight stroke or a very flat angle is not a head.
        assert!(caret(&wobble(&[(0.0, 0.0), (100.0, 0.0)], 20, 0.5, 2)).is_none());
        assert!(caret(&wobble(&[(0.0, 0.0), (50.0, 5.0), (100.0, 0.0)], 20, 0.5, 2)).is_none());
        // Arms of very different length: no.
        assert!(caret(&wobble(&[(0.0, 0.0), (100.0, 50.0), (110.0, 40.0)], 20, 0.5, 2)).is_none());
    }

    /// Straightening: hooks at the ends and a slight bow still make a line; it spans the
    /// stroke, keeps its direction, and is levelled near the axes.
    #[test]
    fn rough_lines_are_straightened_well() {
        let line = |pts: &[Point]| {
            let s = classify(pts).expect("a line");
            assert_eq!(s.kind, ShapeKind::Line, "{pts:?}");
            (s.points[0], s.points[1])
        };
        // A bowed line (bulge 4 % of its length) with a hook where the pen lifted.
        let mut bowed: Vec<Point> = (0..=60)
            .map(|k| {
                let t = f64::from(k) / 60.0;
                Point::new(300.0 * t, 120.0 * t + 12.0 * (t * std::f64::consts::PI).sin())
            })
            .collect();
        bowed.extend([Point::new(296.0, 126.0), Point::new(292.0, 128.0)]);
        let (a, b) = line(&bowed);
        assert!(
            dist(a, Point::new(0.0, 0.0)) < 15.0 && dist(b, Point::new(300.0, 120.0)) < 15.0,
            "{a:?} {b:?}"
        );
        // Drawn right to left: the direction is kept.
        let back: Vec<Point> = bowed.iter().rev().copied().collect();
        let (a2, _) = line(&back);
        assert!(a2.x > 250.0, "{a2:?}");
        // 3° off horizontal: exactly horizontal; 4° off vertical: exactly vertical.
        let tilted = |deg: f64| -> Vec<Point> {
            let r = deg.to_radians();
            (0..=40)
                .map(|k| {
                    let t = f64::from(k) * 5.0;
                    Point::new(t * r.cos() + (f64::from(k) * 1.7).sin(), t * r.sin())
                })
                .collect()
        };
        let (a, b) = line(&tilted(3.0));
        assert!((a.y - b.y).abs() < 1e-9 && (b.x - a.x) > 190.0, "{a:?} {b:?}");
        let (a, b) = line(&tilted(94.0));
        assert!((a.x - b.x).abs() < 1e-9, "{a:?} {b:?}");
        // 30° stays 30°.
        let (a, b) = line(&tilted(30.0));
        let deg = (b.y - a.y).atan2(b.x - a.x).to_degrees();
        assert!((deg - 30.0).abs() < 2.0, "{deg}");
    }

    #[test]
    fn rectangles_ignore_corner_overshoot_and_round_ellipses_are_circles() {
        // Overshoot of 12 px past two corners.
        let r = classify(&wobble(
            &[
                (0.0, 0.0),
                (212.0, 0.0),
                (200.0, 0.0),
                (200.0, 120.0),
                (0.0, 132.0),
                (0.0, 120.0),
                (0.0, 0.0),
            ],
            15,
            1.0,
            3,
        ));
        let r = r.unwrap();
        assert_eq!((r.kind, r.points.len()), (ShapeKind::Rectangle, 2), "{r:?}");
        assert!(r.points[1].x < 210.0 && r.points[1].y < 130.0, "{r:?}");
        let circle: Vec<Point> = (0..=80)
            .map(|k| {
                let t = f64::from(k) / 80.0 * std::f64::consts::TAU;
                Point::new(100.0 + 95.0 * t.cos(), 100.0 + 100.0 * t.sin())
            })
            .collect();
        let c = classify(&circle).unwrap();
        assert_eq!(c.kind, ShapeKind::Ellipse);
        let (w, h) = (c.points[1].x - c.points[0].x, c.points[1].y - c.points[0].y);
        assert!((w - h).abs() < 1e-9, "{w} {h}");
    }

    #[test]
    fn scribbles_and_writing_are_not_shapes() {
        // A "V" and an "L" are not lines.
        let v = [(0.0, 0.0), (50.0, 150.0), (100.0, 0.0)];
        assert_ne!(classify(&wobble(&v, 30, 1.0, 1)).map(|s| s.kind), Some(ShapeKind::Line));
        let l = [(0.0, 0.0), (0.0, 150.0), (100.0, 150.0)];
        assert_ne!(classify(&wobble(&l, 30, 1.0, 1)).map(|s| s.kind), Some(ShapeKind::Line));
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
        // Few samples: still a line.
        let three = [Point::new(0.0, 0.0), Point::new(20.0, 0.5), Point::new(40.0, 0.0)];
        assert_eq!(classify(&three).map(|s| s.kind), Some(ShapeKind::Line));
    }
}
