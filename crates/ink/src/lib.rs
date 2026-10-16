//! Stroke processing and pen input.
//!
//! - [`input`]: pen events as received from the compositor and the versioned ink fixture format
//!   (AGENTS.md §2 Phase 3 item 4).
//! - [`lod`]: levels of detail for drawing zoomed out (ADR 0003, M3.2).
//! - [`lasso`]: lasso selection geometry (R-PEN-009).
//! - [`erase`]: eraser geometry (stroke and area eraser, R-PEN-007/008).

pub mod erase;
pub mod input;
pub mod lasso;
pub mod lod;
pub mod shapes;

/// Pressure → stroke width (canvas units) for a tool's base width (R-PEN-002).
/// Strictly increasing in `p` on [0, 1]; `p` outside is clamped.
pub fn pressure_width(base: f32, p: f32) -> f32 {
    let p = if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 };
    base * (0.25 + 0.75 * p)
}

/// Largest tilt that still widens a tilt-responsive tool (degrees).
pub const TILT_MAX_DEG: f32 = 80.0;

/// Stroke width at one sample for a tool (R-PEN-002, R-PEN-003): the pen follows pressure only;
/// the highlighter, like a chisel tip laid flat, also widens with tilt:
/// `pressure_width · (1 + 1.5 · sin(min(tilt, 80°)))`, tilt = magnitude of (tilt_x, tilt_y).
pub fn tool_width(tool: vibenote_model::Tool, base: f32, p: f32, tilt_x: f32, tilt_y: f32) -> f32 {
    let w = pressure_width(base, p);
    match tool {
        vibenote_model::Tool::Pen => w,
        // A cover is as wide at any pressure: it must hide what it is drawn over.
        vibenote_model::Tool::Cover => base,
        vibenote_model::Tool::Highlighter => {
            let t = tilt_x.hypot(tilt_y);
            let t = if t.is_finite() { t.clamp(0.0, TILT_MAX_DEG) } else { 0.0 };
            w * (1.0 + 1.5 * t.to_radians().sin())
        }
    }
}

#[cfg(test)]
mod width_tests {
    use vibenote_model::Tool;

    #[test]
    fn tilt_widens_the_highlighter_only() {
        let w = |tool, t| super::tool_width(tool, 3.0, 0.6, t, 0.0);
        assert!((w(Tool::Pen, 0.0) - w(Tool::Pen, 60.0)).abs() < f32::EPSILON);
        assert!(w(Tool::Highlighter, 60.0) > 2.2 * w(Tool::Highlighter, 0.0));
        let mut prev = 0.0;
        for t in 0..=90u8 {
            let v = w(Tool::Highlighter, f32::from(t));
            assert!(v >= prev, "monotonic");
            prev = v;
        }
        assert_eq!(w(Tool::Highlighter, f32::NAN), w(Tool::Highlighter, 0.0));
        assert_eq!(w(Tool::Highlighter, 89.0), w(Tool::Highlighter, 80.0), "capped");
    }

    #[test]
    fn width_is_monotonic_and_bounded() {
        let mut prev = 0.0;
        for i in 0..=100u16 {
            let w = super::pressure_width(2.0, f32::from(i) / 100.0);
            assert!(w > prev);
            prev = w;
        }
        assert_eq!(super::pressure_width(2.0, 1.0), 2.0);
        assert_eq!(super::pressure_width(2.0, f32::NAN), 0.5);
    }
}
