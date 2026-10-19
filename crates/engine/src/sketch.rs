//! How shapes are drawn (R-SHAPE-003): clean geometry, or a hand-drawn look (owner 2026-10-19:
//! "autocorrect them but still makes them look a bit hand drawn", like draw.io's sketch style).
//!
//! The hand-drawn look draws every side twice, each time slightly bowed, its ends a little off
//! and overshooting the corners; an ellipse is a loop whose ends overlap, with a gently varying
//! radius. The randomness is seeded by the shape's own size and form (not its position), so a
//! shape looks the same every time it is drawn, while it is previewed and after it is moved.

use vibenote_geom::Point;
use vibenote_model::settings::ShapeStyle;
use vibenote_model::{Color, Sample, ShapeKind, Stroke, Tool, shape_outline};

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [-1, 1].
    fn signed(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)] // 53 bits of randomness
        let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        u.mul_add(2.0, -1.0)
    }
}

/// A seed from the shape's form relative to its first point (moving it keeps its look).
fn seed(kind: ShapeKind, points: &[Point]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ kind as u64;
    let o = points.first().copied().unwrap_or(Point::new(0.0, 0.0));
    for p in points {
        for v in [p.x - o.x, p.y - o.y] {
            // Quarter units: rounding noise does not change the look.
            #[allow(clippy::cast_possible_truncation)] // bounded canvas coordinates
            let q = (v * 4.0).round() as i64;
            h = (h ^ q.cast_unsigned()).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn sample(p: Point) -> Sample {
    Sample {
        x: p.x,
        y: p.y,
        pressure: 1.0,
        tilt_x: 0.0,
        tilt_y: 0.0,
        t_us: 0,
    }
    .quantised()
}

/// One side drawn by hand from `a` to `b`: ends a little off and past the corners, a slight
/// bow.
fn side(a: Point, b: Point, width: f32, r: &mut Rng) -> Vec<Point> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return vec![a, b];
    }
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy, ux);
    let rough = 1.0 + 0.4 * f64::from(width);
    let off = (0.008 * len).min(1.6) * rough;
    let over = (0.03 * len).min(3.5) * rough;
    let bow = r.signed() * (0.018 * len).min(4.5) * rough;
    let wave = r.signed() * (0.006 * len).min(1.5) * rough;
    let a2 = Point::new(
        a.x - ux * over * r.signed().abs() + nx * off * r.signed(),
        a.y - uy * over * r.signed().abs() + ny * off * r.signed(),
    );
    let b2 = Point::new(
        b.x + ux * over * r.signed().abs() + nx * off * r.signed(),
        b.y + uy * over * r.signed().abs() + ny * off * r.signed(),
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 4..=32
    let n = (len / 10.0).clamp(4.0, 32.0) as u32;
    (0..=n)
        .map(|k| {
            let t = f64::from(k) / f64::from(n);
            let o = bow * 4.0 * t * (1.0 - t) + wave * (t * std::f64::consts::TAU).sin();
            Point::new(a2.x + (b2.x - a2.x) * t + nx * o, a2.y + (b2.y - a2.y) * t + ny * o)
        })
        .collect()
}

/// An ellipse drawn by hand inside the box `a`–`b`: a loop starting anywhere whose ends overlap,
/// the radius varying gently.
fn loop_by_hand(a: Point, b: Point, width: f32, r: &mut Rng) -> Vec<Point> {
    let (cx, cy) = (f64::midpoint(a.x, b.x), f64::midpoint(a.y, b.y));
    let (rx, ry) = (((b.x - a.x) / 2.0).abs(), ((b.y - a.y) / 2.0).abs());
    let rough = 1.0 + 0.4 * f64::from(width);
    let amp = (0.025 * rx.max(ry)).min(3.0) * rough / rx.max(ry).max(1e-9);
    let start = r.signed() * std::f64::consts::PI;
    let overlap = 0.25 + 0.2 * r.signed().abs();
    let (h2, h3, p2, p3) = (
        r.signed() * amp,
        r.signed() * amp * 0.6,
        r.signed() * 3.0,
        r.signed() * 3.0,
    );
    let span = std::f64::consts::TAU + overlap;
    (0..=72)
        .map(|k| {
            let t = start + span * f64::from(k) / 72.0;
            let m = 1.0 + h2 * (2.0 * t + p2).sin() + h3 * (3.0 * t + p3).sin();
            Point::new(cx + rx * m * t.cos(), cy + ry * m * t.sin())
        })
        .collect()
}

/// The strokes that draw a shape in `style` (overlay and canvas drawing, the eraser's view of a
/// shape).
pub fn shape_strokes(
    style: ShapeStyle,
    kind: ShapeKind,
    points: &[Point],
    closed: bool,
    color: Color,
    width: f32,
) -> Vec<Stroke> {
    let stroke = |pts: Vec<Point>, w: f32| Stroke {
        tool: Tool::Pen,
        color,
        width: w,
        samples: pts.into_iter().map(sample).collect(),
    };
    let lines = shape_outline(kind, points, closed, width);
    if style == ShapeStyle::Clean {
        return lines.into_iter().map(|l| stroke(l, width)).collect();
    }
    let mut r = Rng(seed(kind, points));
    // Two thinner passes weigh about as much as one clean line.
    let w = width * 0.8;
    let mut out = Vec::new();
    if kind == ShapeKind::Ellipse {
        if let (Some(&a), Some(&b)) = (points.first(), points.get(1)) {
            for _ in 0..2 {
                out.push(stroke(loop_by_hand(a, b, width, &mut r), w));
            }
        }
        return out;
    }
    for l in lines {
        for pair in l.windows(2) {
            if let (Some(&a), Some(&b)) = (pair.first(), pair.get(1)) {
                for _ in 0..2 {
                    out.push(stroke(side(a, b, width, &mut r), w));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Vec<Point> {
        vec![Point::new(0.0, 0.0), Point::new(200.0, 100.0)]
    }

    #[test]
    fn clean_is_the_outline_and_hand_drawn_stays_near_it() {
        let c = shape_strokes(ShapeStyle::Clean, ShapeKind::Rectangle, &rect(), true, Color::INK, 2.0);
        assert_eq!(c.len(), 1);
        let h = shape_strokes(
            ShapeStyle::HandDrawn,
            ShapeKind::Rectangle,
            &rect(),
            true,
            Color::INK,
            2.0,
        );
        assert_eq!(h.len(), 8, "each side twice");
        let mut bent = 0.0f64;
        for s in &h {
            for p in &s.samples {
                // Distance to the nearest side's line.
                let on_side =
                    p.y.abs()
                        .min((p.y - 100.0).abs())
                        .min(p.x.abs())
                        .min((p.x - 200.0).abs());
                assert!(on_side < 8.0, "{p:?} strays {on_side}");
                bent = bent.max(on_side);
            }
        }
        assert!(bent > 0.5, "it is not a clean line: {bent}");
    }

    #[test]
    fn the_look_depends_on_the_form_not_the_position() {
        let at = |dx: f64| {
            let p: Vec<Point> = rect().iter().map(|q| Point::new(q.x + dx, q.y)).collect();
            shape_strokes(ShapeStyle::HandDrawn, ShapeKind::Rectangle, &p, true, Color::INK, 2.0)
        };
        let (a, b) = (at(0.0), at(500.0));
        for (s, t) in a.iter().zip(&b) {
            for (p, q) in s.samples.iter().zip(&t.samples) {
                assert!((p.x + 500.0 - q.x).abs() < 0.01 && (p.y - q.y).abs() < 0.01);
            }
        }
        let e = shape_strokes(
            ShapeStyle::HandDrawn,
            ShapeKind::Ellipse,
            &rect(),
            true,
            Color::INK,
            2.0,
        );
        assert_eq!(e.len(), 2);
        for s in &e {
            for p in &s.samples {
                let r = ((p.x - 100.0) / 100.0).hypot((p.y - 50.0) / 50.0);
                assert!((r - 1.0).abs() < 0.08, "{r}");
            }
        }
    }
}
