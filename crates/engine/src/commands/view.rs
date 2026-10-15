//! View commands (R-CANVAS-002): pan, zoom 1 %–6,400 % anchored at a surface point, zoom to fit.
//! The view is session state (not undoable, not stored). Surface coordinates are logical px.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session, View};
use schemars::JsonSchema;
use serde::Deserialize;
use vibenote_geom::{Point, Rect};
use vibenote_model::ObjectKind;

/// Zoom range (R-CANVAS-002: 1 %–6,400 %).
pub const ZOOM_MIN: f64 = 0.01;
pub const ZOOM_MAX: f64 = 64.0;
/// Largest pan step or surface coordinate accepted (hostile input).
const SURFACE_MAX: f64 = 1.0e6;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Pan {
    /// Surface px the content moves right / down.
    dx: f64,
    dy: f64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Zoom {
    /// Multiplies the zoom; the result is clamped to 1 %–6,400 %.
    factor: f64,
    /// Surface point that stays put (default: the surface origin).
    anchor: Option<[f64; 2]>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Step {
    /// Surface point that stays put (default: the surface origin).
    anchor: Option<[f64; 2]>,
}

/// Zoom step of `view.zoom_in` / `view.zoom_out` (keyboard).
const ZOOM_STEP: f64 = 1.25;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Fit {
    /// Visible surface size in logical px (default 1920×1200).
    viewport: Option<[f64; 2]>,
}

fn surface(v: f64, what: &str) -> Result<f64, EngineError> {
    if v.is_finite() && v.abs() <= SURFACE_MAX {
        Ok(v)
    } else {
        Err(EngineError::Model(format!("{what} must be finite and within ±1e6")))
    }
}

/// The view after zooming by `factor` around surface point `(ax, ay)`.
pub(crate) fn zoomed(v: View, factor: f64, (ax, ay): (f64, f64)) -> View {
    let zoom = (v.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
    let (cx, cy) = v.to_canvas(ax, ay);
    View {
        origin_x: cx - ax / zoom,
        origin_y: cy - ay / zoom,
        zoom,
    }
}

/// Bounds of a note's content: objects in memory plus, for a note still loading, the stored ones.
fn content_bounds(s: &Session, note: vibenote_model::NoteId) -> Result<Rect, EngineError> {
    let mut r = Rect::EMPTY;
    for o in s.ws.canvas(note)?.objects.values() {
        if let ObjectKind::Stroke(st) = &o.kind {
            let b = st.bounds();
            if !b.is_empty() {
                r.include(b.min);
                r.include(b.max);
            }
        }
    }
    if let Some(b) = s.stored_bounds(note)? {
        r.include(b.min);
        r.include(b.max);
    }
    Ok(r)
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "view.pan",
            doc: "Pan the view by a surface offset (logical px).",
            reqs: &["R-CANVAS-002"],
            implemented: true,
        },
        |s: &mut Session, p: Pan| {
            let (dx, dy) = (surface(p.dx, "dx")?, surface(p.dy, "dy")?);
            let v = s.view;
            s.view = View {
                origin_x: v.origin_x - dx / v.zoom,
                origin_y: v.origin_y - dy / v.zoom,
                ..v
            };
            Ok(s.view)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "view.zoom",
            doc: "Zoom by a factor around a surface point (1 %–6,400 %; the point stays under the cursor).",
            reqs: &["R-CANVAS-002"],
            implemented: true,
        },
        |s: &mut Session, p: Zoom| {
            if !(p.factor.is_finite() && p.factor > 0.0) {
                return Err(EngineError::Model("factor must be positive and finite".into()));
            }
            let [ax, ay] = p.anchor.unwrap_or([0.0, 0.0]);
            let anchor = (surface(ax, "anchor")?, surface(ay, "anchor")?);
            s.view = zoomed(s.view, p.factor, anchor);
            Ok(s.view)
        },
    )))?;
    for (id, doc, step) in [
        (
            "view.zoom_in",
            "Zoom in one step (×1.25) around a surface point.",
            Some(ZOOM_STEP),
        ),
        (
            "view.zoom_out",
            "Zoom out one step (÷1.25) around a surface point.",
            Some(1.0 / ZOOM_STEP),
        ),
        (
            "view.zoom_reset",
            "Zoom to 100 % around a surface point (Ctrl+1).",
            None,
        ),
    ] {
        r.register(Box::new(Typed::new(
            Spec {
                id,
                doc,
                reqs: &["R-CANVAS-002", "R-CMD-002"],
                implemented: true,
            },
            move |s: &mut Session, p: Step| {
                let [ax, ay] = p.anchor.unwrap_or([0.0, 0.0]);
                let anchor = (surface(ax, "anchor")?, surface(ay, "anchor")?);
                let factor = step.unwrap_or(1.0 / s.view.zoom);
                if !(factor.is_finite() && factor > 0.0) {
                    return Err(EngineError::Model("invalid zoom".into()));
                }
                s.view = zoomed(s.view, factor, anchor);
                Ok(s.view)
            },
        )))?;
    }
    r.register(Box::new(Typed::new(
        Spec {
            id: "view.zoom_fit",
            doc: "Zoom to fit the current note's content into the surface (5 % margin; empty note: 100 % at the origin).",
            reqs: &["R-CANVAS-002", "R-CMD-002"],
            implemented: true,
        },
        |s: &mut Session, p: Fit| {
            let note = s
                .current_note
                .ok_or_else(|| EngineError::Model("no current note".into()))?;
            let [w, h] = p.viewport.unwrap_or([1920.0, 1200.0]);
            let (w, h) = (surface(w, "viewport")?, surface(h, "viewport")?);
            if w < 1.0 || h < 1.0 {
                return Err(EngineError::Model("viewport must be at least 1×1 px".into()));
            }
            let b = content_bounds(s, note)?;
            s.view = if b.is_empty() {
                View::default()
            } else {
                let (bw, bh) = ((b.max.x - b.min.x).max(1e-6), (b.max.y - b.min.y).max(1e-6));
                let zoom = (w / (bw * 1.1)).min(h / (bh * 1.1)).clamp(ZOOM_MIN, ZOOM_MAX);
                let c = Point::new(f64::midpoint(b.min.x, b.max.x), f64::midpoint(b.min.y, b.max.y));
                View {
                    origin_x: c.x - w / 2.0 / zoom,
                    origin_y: c.y - h / 2.0 / zoom,
                    zoom,
                }
            };
            Ok(s.view)
        },
    )))?;
    Ok(())
}
