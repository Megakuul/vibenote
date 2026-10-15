//! Geometry primitives for an infinite canvas.
//!
//! Canvas coordinates are `f64` (R-CANVAS-001: precise at ±1e7 units); renderers convert to
//! chunk-relative `f32` (ADR 0003).

use serde::{Deserialize, Serialize};

/// A point in canvas units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn distance(self, o: Point) -> f64 {
        (self.x - o.x).hypot(self.y - o.y)
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// Axis-aligned rectangle (min inclusive, max inclusive). An "empty" rect has min > max.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

impl Rect {
    pub const EMPTY: Rect = Rect {
        min: Point::new(f64::INFINITY, f64::INFINITY),
        max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
    };
    pub fn new(a: Point, b: Point) -> Self {
        Rect {
            min: Point::new(a.x.min(b.x), a.y.min(b.y)),
            max: Point::new(a.x.max(b.x), a.y.max(b.y)),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }
    pub fn include(&mut self, p: Point) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let mut r = *self;
        if !o.is_empty() {
            r.include(o.min);
            r.include(o.max);
        }
        r
    }
    pub fn inflate(&self, d: f64) -> Rect {
        if self.is_empty() {
            return *self;
        }
        Rect {
            min: Point::new(self.min.x - d, self.min.y - d),
            max: Point::new(self.max.x + d, self.max.y + d),
        }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.min.x <= o.max.x
            && o.min.x <= self.max.x
            && self.min.y <= o.max.y
            && o.min.y <= self.max.y
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
    pub fn width(&self) -> f64 {
        if self.is_empty() { 0.0 } else { self.max.x - self.min.x }
    }
    pub fn height(&self) -> f64 {
        if self.is_empty() { 0.0 } else { self.max.y - self.min.y }
    }
}

/// 2D affine transform: x' = a·x + c·y + e, y' = b·x + d·y + f.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Affine {
    fn default() -> Self {
        Affine::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };
    pub fn translate(dx: f64, dy: f64) -> Self {
        Affine {
            e: dx,
            f: dy,
            ..Affine::IDENTITY
        }
    }
    pub fn scale(s: f64) -> Self {
        Affine {
            a: s,
            d: s,
            ..Affine::IDENTITY
        }
    }
    pub fn rotate(rad: f64) -> Self {
        let (s, c) = rad.sin_cos();
        Affine {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }
    /// `self ∘ o` (apply `o` first, then `self`).
    pub fn then_apply(&self, o: &Affine) -> Affine {
        Affine {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    /// Inverse, or `None` if (nearly) singular.
    pub fn inverse(&self) -> Option<Affine> {
        let det = self.determinant();
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine {
            a,
            b,
            c,
            d,
            e: -(a * self.e + c * self.f),
            f: -(b * self.e + d * self.f),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse_roundtrip() {
        let t = Affine::translate(3.0, -7.0)
            .then_apply(&Affine::rotate(0.3))
            .then_apply(&Affine::scale(2.5));
        let inv = t.inverse().unwrap();
        let p = Point::new(1e7, -2.5e6);
        let q = inv.apply(t.apply(p));
        assert!((q.x - p.x).abs() < 1e-6 && (q.y - p.y).abs() < 1e-6, "{q:?}");
    }

    #[test]
    fn singular_affine_has_no_inverse() {
        assert!(Affine::scale(0.0).inverse().is_none());
    }

    #[test]
    fn rect_union_and_intersection() {
        let a = Rect::new(Point::new(0.0, 0.0), Point::new(10.0, 10.0));
        let b = Rect::new(Point::new(5.0, 5.0), Point::new(20.0, 20.0));
        assert!(a.intersects(&b));
        assert_eq!(a.union(&b).max, Point::new(20.0, 20.0));
        assert!(!Rect::EMPTY.intersects(&a));
        assert_eq!(Rect::EMPTY.union(&a), a);
    }
}
