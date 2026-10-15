//! Eraser geometry (R-PEN-007 stroke eraser, R-PEN-008 area eraser).
//!
//! The eraser is a polyline swept by a disc of radius `radius`. A stroke's ink at parameter `t`
//! of a segment a→b is a disc of the stroke's radius (interpolated like the renderer). Ink is
//! under the eraser where `dist(P(t), eraser) ≤ radius + r(t)`. Along a segment, the distance to
//! a convex set is convex and `r(t)` is linear, so each eraser segment covers one interval of
//! `t`; it is found with a ternary search for the minimum and bisection for the ends.
//!
//! Bounded for hostile input: eraser paths are capped at [`MAX_ERASER_POINTS`], and a stroke
//! segment is only tested against eraser segments whose boxes are within reach.

use crate::tool_width;
use vibenote_model::{Sample, Stroke};

/// Longest eraser path accepted (points).
pub const MAX_ERASER_POINTS: usize = 4096;
const ITERATIONS: u32 = 48;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eraser<'a> {
    /// Path in canvas units.
    pub path: &'a [(f64, f64)],
    /// Disc radius, canvas units.
    pub radius: f64,
}

impl Eraser<'_> {
    /// Rejects hostile paths (too long, non-finite, negative radius).
    pub fn check(&self) -> Result<(), String> {
        if self.path.is_empty() || self.path.len() > MAX_ERASER_POINTS {
            return Err(format!("eraser path must have 1..={MAX_ERASER_POINTS} points"));
        }
        let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
        if !self.path.iter().all(|&(x, y)| ok(x) && ok(y)) {
            return Err("eraser path out of range".into());
        }
        if !(self.radius.is_finite() && self.radius >= 0.0 && self.radius <= 1.0e6) {
            return Err("eraser radius out of range".into());
        }
        Ok(())
    }

    fn segments(&self) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
        let first = self.path.first().copied();
        first
            .into_iter()
            .map(|p| (p, p))
            .chain(self.path.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))))
    }
}

fn dist_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - dx * t).hypot(p.1 - a.1 - dy * t)
}

/// One stroke segment: ends and radii.
#[derive(Clone, Copy)]
struct Seg {
    a: (f64, f64),
    b: (f64, f64),
    ra: f64,
    rb: f64,
}

impl Seg {
    fn at(&self, t: f64) -> (f64, f64) {
        (
            self.a.0 + (self.b.0 - self.a.0) * t,
            self.a.1 + (self.b.1 - self.a.1) * t,
        )
    }
    fn reach(&self) -> f64 {
        self.ra.max(self.rb)
    }
}

/// The interval of `t` where the segment's ink meets the eraser segment e0–e1, if any.
fn covered_by(s: &Seg, e0: (f64, f64), e1: (f64, f64), radius: f64) -> Option<(f64, f64)> {
    let f = |t: f64| dist_to_segment(s.at(t), e0, e1) - radius - (s.ra + (s.rb - s.ra) * t);
    // Minimum of a convex function on [0, 1].
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..ITERATIONS {
        let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
        if f(m1) <= f(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let tm = f64::midpoint(lo, hi);
    let fm = f(tm);
    if fm > 0.0 {
        return None;
    }
    // Ends: f ≤ 0 on [t0, t1] (sublevel set of a convex function).
    let edge = |mut inside: f64, mut outside: f64| {
        for _ in 0..ITERATIONS {
            let m = f64::midpoint(inside, outside);
            if f(m) <= 0.0 {
                inside = m;
            } else {
                outside = m;
            }
        }
        inside
    };
    let t0 = if f(0.0) <= 0.0 { 0.0 } else { edge(tm, 0.0) };
    let t1 = if f(1.0) <= 0.0 { 1.0 } else { edge(tm, 1.0) };
    Some((t0, t1))
}

fn segs(stroke: &Stroke) -> Vec<Seg> {
    let r = |p: &Sample| f64::from(tool_width(stroke.tool, stroke.width, p.pressure, p.tilt_x, p.tilt_y)) * 0.5;
    let mut out = Vec::with_capacity(stroke.samples.len());
    let mut prev: Option<&Sample> = None;
    for p in &stroke.samples {
        let a = prev.unwrap_or(p);
        out.push(Seg {
            a: (a.x, a.y),
            b: (p.x, p.y),
            ra: r(a),
            rb: r(p),
        });
        prev = Some(p);
    }
    out
}

/// Merged covered intervals of each segment (segment `j` runs from sample `j-1` to `j`; segment
/// 0 is the first sample's dot).
fn coverage(stroke: &Stroke, e: &Eraser<'_>) -> Vec<Vec<(f64, f64)>> {
    let ssegs = segs(stroke);
    // Only eraser segments near the whole stroke matter (cheap first cut for long paths).
    let (mut sx0, mut sy0, mut sx1, mut sy1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for s in &ssegs {
        let reach = e.radius + s.reach();
        sx0 = sx0.min(s.a.0.min(s.b.0) - reach);
        sy0 = sy0.min(s.a.1.min(s.b.1) - reach);
        sx1 = sx1.max(s.a.0.max(s.b.0) + reach);
        sy1 = sy1.max(s.a.1.max(s.b.1) + reach);
    }
    let (esegs, boxes): (Vec<_>, Vec<(f64, f64, f64, f64)>) = e
        .segments()
        .map(|(p, q)| ((p, q), (p.0.min(q.0), p.1.min(q.1), p.0.max(q.0), p.1.max(q.1))))
        .filter(|(_, b)| b.0 <= sx1 && b.2 >= sx0 && b.1 <= sy1 && b.3 >= sy0)
        .unzip();
    if esegs.is_empty() {
        return vec![Vec::new(); ssegs.len()];
    }
    ssegs
        .iter()
        .map(|s| {
            let reach = e.radius + s.reach();
            let (x0, y0) = (s.a.0.min(s.b.0) - reach, s.a.1.min(s.b.1) - reach);
            let (x1, y1) = (s.a.0.max(s.b.0) + reach, s.a.1.max(s.b.1) + reach);
            let mut iv: Vec<(f64, f64)> = esegs
                .iter()
                .zip(&boxes)
                .filter(|(_, b)| b.0 <= x1 && b.2 >= x0 && b.1 <= y1 && b.3 >= y0)
                .filter_map(|((p, q), _)| covered_by(s, *p, *q, e.radius))
                .collect();
            iv.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f64, f64)> = Vec::new();
            for (a, b) in iv {
                match merged.last_mut() {
                    Some(last) if a <= last.1 => last.1 = last.1.max(b),
                    _ => merged.push((a, b)),
                }
            }
            merged
        })
        .collect()
}

/// True if any of the stroke's ink is under the eraser (stroke eraser, R-PEN-007).
pub fn touches(stroke: &Stroke, e: &Eraser<'_>) -> bool {
    coverage(stroke, e).iter().any(|iv| !iv.is_empty())
}

fn lerp(a: &Sample, b: &Sample, t: f64) -> Sample {
    #[allow(clippy::cast_possible_truncation)] // t ∈ [0, 1]
    let tf = t as f32;
    let dt = b.t_us.saturating_sub(a.t_us);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let add = (dt as f64 * t).round().clamp(0.0, dt as f64) as u64;
    Sample {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        pressure: a.pressure + (b.pressure - a.pressure) * tf,
        tilt_x: a.tilt_x + (b.tilt_x - a.tilt_x) * tf,
        tilt_y: a.tilt_y + (b.tilt_y - a.tilt_y) * tf,
        t_us: a.t_us.saturating_add(add),
    }
    .quantised()
}

/// Area eraser (R-PEN-008): the parts of the stroke not under the eraser, as sample runs (each
/// becomes a stroke). `None` if the eraser does not touch the stroke; an empty list if it covers
/// all of it. Pieces keep the original samples (pressure, tilt, time); where a piece is cut, a
/// sample is interpolated on the boundary.
pub fn split(stroke: &Stroke, e: &Eraser<'_>) -> Option<Vec<Vec<Sample>>> {
    let cov = coverage(stroke, e);
    if cov.iter().all(Vec::is_empty) {
        return None;
    }
    let mut pieces: Vec<Vec<Sample>> = Vec::new();
    let mut cur: Vec<Sample> = Vec::new();
    let close = |cur: &mut Vec<Sample>, pieces: &mut Vec<Vec<Sample>>| {
        if !cur.is_empty() {
            pieces.push(std::mem::take(cur));
        }
    };
    for (j, p) in stroke.samples.iter().enumerate() {
        let iv = cov.get(j).map_or(&[][..], Vec::as_slice);
        let Some(a) = j.checked_sub(1).and_then(|i| stroke.samples.get(i)) else {
            // The first sample (a dot): kept unless covered.
            if iv.is_empty() {
                cur.push(*p);
            }
            continue;
        };
        // Walk the segment a→p, cutting out each covered interval.
        let mut t = 0.0f64;
        for &(t0, t1) in iv {
            if t0 > t && !cur.is_empty() {
                // ink continues up to t0, then is cut there
                if t0 < 1.0 {
                    cur.push(lerp(a, p, t0));
                }
            } else if t0 > t && cur.is_empty() && t0 > 0.0 {
                // a piece starts at t (a previous cut) and runs to t0
                cur.push(lerp(a, p, t));
                cur.push(lerp(a, p, t0));
            }
            close(&mut cur, &mut pieces);
            t = t1;
        }
        if t < 1.0 {
            if cur.is_empty() && t > 0.0 {
                cur.push(lerp(a, p, t));
            }
            cur.push(*p);
        }
    }
    close(&mut cur, &mut pieces);
    // A single point left at a cut is not ink.
    pieces.retain(|s| s.len() >= 2 || stroke.samples.len() == 1);
    Some(pieces)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use vibenote_model::{Color, Tool};

    fn line(x0: f64, x1: f64, n: usize, w: f32) -> Stroke {
        Stroke {
            tool: Tool::Pen,
            color: Color::INK,
            width: w,
            samples: (0..n)
                .map(|i| {
                    let u = i as f64 / (n - 1).max(1) as f64;
                    Sample {
                        x: x0 + (x1 - x0) * u,
                        y: 0.0,
                        pressure: 1.0,
                        tilt_x: 0.0,
                        tilt_y: 0.0,
                        t_us: i as u64 * 1000,
                    }
                    .quantised()
                })
                .collect(),
        }
    }

    #[test]
    fn a_crossing_eraser_splits_a_line_in_two() {
        let s = line(0.0, 100.0, 3, 2.0); // samples at 0, 50, 100: the cut falls between them
        let e = Eraser {
            path: &[(50.0, -20.0), (50.0, 20.0)],
            radius: 5.0,
        };
        assert!(touches(&s, &e));
        let p = split(&s, &e).unwrap();
        assert_eq!(p.len(), 2);
        // ink radius 1 (width 2): cut where the ink meets the eraser, |x − 50| = 6
        assert!((p[0].last().unwrap().x - 44.0).abs() < 1e-3, "{:?}", p[0]);
        assert!((p[1].first().unwrap().x - 56.0).abs() < 1e-3, "{:?}", p[1]);
        assert_eq!(p[0].first().unwrap().x, 0.0);
        assert_eq!(p[1].last().unwrap().x, 100.0);
    }

    #[test]
    fn untouched_and_fully_covered() {
        let s = line(0.0, 10.0, 5, 2.0);
        let far = Eraser {
            path: &[(500.0, 500.0)],
            radius: 5.0,
        };
        assert!(!touches(&s, &far));
        assert_eq!(split(&s, &far), None);
        let all = Eraser {
            path: &[(5.0, 0.0)],
            radius: 50.0,
        };
        assert_eq!(split(&s, &all), Some(vec![]));
    }

    #[test]
    fn hostile_paths_are_rejected() {
        let long = vec![(0.0, 0.0); MAX_ERASER_POINTS + 1];
        assert!(
            Eraser {
                path: &long,
                radius: 1.0
            }
            .check()
            .is_err()
        );
        assert!(
            Eraser {
                path: &[(f64::NAN, 0.0)],
                radius: 1.0
            }
            .check()
            .is_err()
        );
        assert!(
            Eraser {
                path: &[(0.0, 0.0)],
                radius: -1.0
            }
            .check()
            .is_err()
        );
        assert!(Eraser { path: &[], radius: 1.0 }.check().is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        /// What remains is never under the eraser, and every original sample that is not under
        /// the eraser survives in a piece.
        #[test]
        fn pieces_avoid_the_eraser_and_keep_the_rest(
            pts in prop::collection::vec((-50.0f64..50.0, -50.0f64..50.0), 2..40),
            path in prop::collection::vec((-60.0f64..60.0, -60.0f64..60.0), 1..6),
            radius in 0.5f64..15.0,
        ) {
            let s = Stroke {
                tool: Tool::Pen,
                color: Color::INK,
                width: 2.0,
                samples: pts.iter().enumerate().map(|(i, &(x, y))| Sample { x, y, pressure: 0.5, tilt_x: 0.0, tilt_y: 0.0, t_us: i as u64 }.quantised()).collect(),
            };
            let e = Eraser { path: &path, radius };
            let r = |p: &Sample| f64::from(crate::pressure_width(2.0, p.pressure)) * 0.5;
            let d = |p: &Sample| e.segments().map(|(a, b)| dist_to_segment((p.x, p.y), a, b)).fold(f64::INFINITY, f64::min);
            match split(&s, &e) {
                None => prop_assert!(s.samples.iter().all(|p| d(p) > radius + r(p) - 1e-6)),
                Some(pieces) => {
                    for piece in &pieces {
                        for p in piece {
                            prop_assert!(d(p) >= radius + r(p) - 1e-2, "sample under the eraser: {:?} d={}", p, d(p));
                        }
                    }
                    for p in &s.samples {
                        if d(p) > radius + r(p) + 1e-2 {
                            prop_assert!(pieces.iter().flatten().any(|q| q == p), "kept sample lost: {:?}", p);
                        }
                    }
                }
            }
        }
    }
}
