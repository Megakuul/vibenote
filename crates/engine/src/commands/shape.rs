//! Shapes (R-SHAPE-001/002) and the ruler (R-RULER-001).

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::Point;
use vibenote_model::{CanvasObject, Color, ObjectId, ObjectKind, Op, ShapeKind};

/// Hold time that turns a rough stroke into a shape [SPEC proposal: 500 ms].
pub const SNAP_HOLD_MS: u64 = 500;
/// Strokes starting this close to the ruler edge (screen px) snap to it.
pub const RULER_SNAP_PX: f64 = 12.0;

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum KindP {
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Polygon,
    Triangle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateP {
    kind: KindP,
    /// Line/arrow ends, or opposite corners of a rectangle/ellipse.
    from: Option<[f64; 2]>,
    to: Option<[f64; 2]>,
    /// Corners of a polygon/triangle.
    points: Option<Vec<[f64; 2]>>,
    /// 0xRRGGBBAA (default ink).
    color: Option<u32>,
    /// Line width in canvas units (default 2).
    width: Option<f32>,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SnapP {
    stroke: u64,
    /// How long the pen was held still at the end of the stroke.
    held_ms: u64,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RulerP {
    /// Show or hide (default: toggle).
    visible: Option<bool>,
    /// A point on the ruler edge (canvas).
    at: Option<[f64; 2]>,
    angle_deg: Option<f64>,
    /// Round the angle to 15° steps (rotating with Shift).
    #[serde(default)]
    snap: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoP {}

#[derive(Serialize)]
struct Created {
    id: u64,
}

#[derive(Serialize)]
struct Snapped {
    /// The recognised shape, or null (nothing changed).
    kind: Option<ShapeKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
}

/// The ruler (session state, not a canvas object).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Ruler {
    pub visible: bool,
    pub x: f64,
    pub y: f64,
    pub angle_deg: f64,
}

impl Ruler {
    /// Projects `p` onto the ruler edge.
    pub fn project(&self, p: (f64, f64)) -> (f64, f64) {
        let (s, c) = self.angle_deg.to_radians().sin_cos();
        let t = (p.0 - self.x) * c + (p.1 - self.y) * s;
        (self.x + t * c, self.y + t * s)
    }

    /// Distance from `p` to the ruler edge.
    pub fn distance(&self, p: (f64, f64)) -> f64 {
        let q = self.project(p);
        (p.0 - q.0).hypot(p.1 - q.1)
    }
}

/// The ruler's size on screen (px): body depth and length; the rotate knob's radius.
pub const RULER_DEPTH_PX: f64 = 72.0;
pub const RULER_LENGTH_PX: f64 = 1800.0;
pub const RULER_KNOB_PX: f64 = 13.0;
/// Colours: the translucent body, its drawing edge and ticks.
const RULER_BODY: vibenote_model::Color = vibenote_model::Color(0x9db4_d64c);
const RULER_EDGE: vibenote_model::Color = vibenote_model::Color(0x345a_8fe0);

/// The part of the ruler under a press: its body (drag to move) or the knob (drag to rotate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RulerPart {
    Body,
    Knob,
}

/// A ruler being moved or rotated by a press on it (R-RULER-001).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RulerDrag {
    pub part: RulerPart,
    /// The press (canvas units) and the ruler then.
    pub start: (f64, f64),
    pub ruler: Ruler,
    pub mouse: bool,
}

impl Ruler {
    /// Unit vectors along the edge and into the body.
    fn axes(&self) -> ((f64, f64), (f64, f64)) {
        let (s, c) = self.angle_deg.to_radians().sin_cos();
        ((c, s), (-s, c))
    }

    /// The rotate knob's centre (canvas units) at `zoom`.
    pub fn knob(&self, zoom: f64) -> (f64, f64) {
        let ((ux, uy), (nx, ny)) = self.axes();
        let along = (RULER_LENGTH_PX / 2.0 - 2.5 * RULER_KNOB_PX) / zoom;
        let across = RULER_DEPTH_PX / 2.0 / zoom;
        (self.x + ux * along + nx * across, self.y + uy * along + ny * across)
    }

    /// The part under canvas point `p` (the band along the edge where strokes snap is not part
    /// of it: drawing starts there).
    pub fn part_at(&self, p: (f64, f64), zoom: f64) -> Option<RulerPart> {
        if !self.visible || !(zoom.is_finite() && zoom > 0.0) {
            return None;
        }
        let k = self.knob(zoom);
        if (p.0 - k.0).hypot(p.1 - k.1) <= (RULER_KNOB_PX + 4.0) / zoom {
            return Some(RulerPart::Knob);
        }
        let ((ux, uy), (nx, ny)) = self.axes();
        let (dx, dy) = (p.0 - self.x, p.1 - self.y);
        let along = dx * ux + dy * uy;
        let across = dx * nx + dy * ny;
        let inside = along.abs() <= RULER_LENGTH_PX / 2.0 / zoom
            && across > RULER_SNAP_PX / zoom
            && across <= RULER_DEPTH_PX / zoom;
        inside.then_some(RulerPart::Body)
    }

    /// The ruler as overlay strokes at `zoom`: body, edge, ticks and the knob.
    pub fn strokes(&self, zoom: f64) -> Vec<vibenote_model::Stroke> {
        let mut out = Vec::new();
        if !self.visible || !(zoom.is_finite() && zoom > 0.0) {
            return out;
        }
        let ((ux, uy), (nx, ny)) = self.axes();
        let at = |along: f64, across: f64| (self.x + ux * along + nx * across, self.y + uy * along + ny * across);
        #[allow(clippy::cast_possible_truncation)] // widths in canvas units
        let seg = |a: (f64, f64), b: (f64, f64), w: f64, color: vibenote_model::Color| vibenote_model::Stroke {
            tool: vibenote_model::Tool::Pen,
            color,
            width: w as f32,
            samples: [a, b]
                .iter()
                .map(|&(x, y)| vibenote_model::Sample {
                    x,
                    y,
                    pressure: 1.0,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: 0,
                })
                .collect(),
        };
        let (half, depth) = (RULER_LENGTH_PX / 2.0 / zoom, RULER_DEPTH_PX / zoom);
        // The body: one wide segment (no overlaps, an even tint).
        out.push(seg(at(-half, depth / 2.0), at(half, depth / 2.0), depth, RULER_BODY));
        out.push(seg(at(-half, 0.0), at(half, 0.0), 1.5 / zoom, RULER_EDGE));
        // Ticks every 1, 2 or 5 × 10^k canvas units, at least 8 px apart; longer every 5th/10th.
        let min = 8.0 / zoom;
        let mut step = 10f64.powf(min.log10().floor());
        for m in [1.0, 2.0, 5.0, 10.0] {
            if step * m >= min {
                step *= m;
                break;
            }
        }
        if step.is_finite() && step > 0.0 {
            #[allow(clippy::cast_possible_truncation)] // a few hundred ticks
            let n = (half / step).floor().min(400.0) as i64;
            for i in n.saturating_neg()..=n {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f64 * step;
                let len = if i.rem_euclid(10) == 0 {
                    22.0
                } else if i.rem_euclid(5) == 0 {
                    15.0
                } else {
                    9.0
                };
                out.push(seg(at(t, 0.0), at(t, len / zoom), 1.0 / zoom, RULER_EDGE));
            }
        }
        // The rotate knob: a ring.
        let (kx, ky) = self.knob(zoom);
        let r = RULER_KNOB_PX / zoom;
        let mut ring = Vec::new();
        for k in 0..=24 {
            let a = f64::from(k) / 24.0 * std::f64::consts::TAU;
            ring.push((kx + r * a.cos(), ky + r * a.sin()));
        }
        for w in ring.windows(2) {
            if let (Some(&a), Some(&b)) = (w.first(), w.get(1)) {
                out.push(seg(a, b, 2.0 / zoom, RULER_EDGE));
            }
        }
        out
    }
}

/// Moves or rotates the ruler being dragged to canvas point `p`.
pub(crate) fn ruler_drag_to(s: &mut Session, p: (f64, f64)) {
    let Some(d) = s.live.ruler_drag else { return };
    let r0 = d.ruler;
    let mut r = s.ruler;
    match d.part {
        RulerPart::Body => {
            r.x = r0.x + (p.0 - d.start.0);
            r.y = r0.y + (p.1 - d.start.1);
        }
        RulerPart::Knob => {
            let a0 = (d.start.1 - r0.y).atan2(d.start.0 - r0.x);
            let a1 = (p.1 - r0.y).atan2(p.0 - r0.x);
            r.angle_deg = (r0.angle_deg + (a1 - a0).to_degrees()).rem_euclid(360.0);
        }
    }
    let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
    if ok(r.x) && ok(r.y) && r.angle_deg.is_finite() {
        s.ruler = r;
    }
}

/// A stroke starting near the visible ruler's edge goes along it (all samples projected).
pub(crate) fn ruler_snap(s: &Session, stroke: &mut vibenote_model::Stroke) {
    let r = s.ruler;
    let zoom = if s.view.zoom.is_finite() && s.view.zoom > 0.0 {
        s.view.zoom
    } else {
        1.0
    };
    let Some(first) = stroke.samples.first() else { return };
    if !r.visible || r.distance((first.x, first.y)) > RULER_SNAP_PX / zoom {
        return;
    }
    for p in &mut stroke.samples {
        (p.x, p.y) = r.project((p.x, p.y));
    }
}

fn pt(p: [f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

/// Inserts a shape on top of `note` (one undo step); returns its id.
pub(crate) fn create(
    s: &mut Session,
    note: vibenote_model::NoteId,
    shape: ShapeKind,
    points: Vec<Point>,
    closed: bool,
    color: Color,
    width: f32,
) -> Result<u64, EngineError> {
    let z = s.next_z(note)?;
    let id = ObjectId(s.ws.alloc_id()?);
    let object = CanvasObject {
        id,
        z,
        kind: ObjectKind::Shape {
            shape,
            points,
            closed,
            color,
            width,
        },
    };
    s.commit(Scope::Note(note), "shape", vec![Op::InsertObject { note, object }])?;
    Ok(id.0)
}

/// Replaces stroke `stroke` by the clean shape it looks like (one undo step back to the ink);
/// `None` if it does not look like one (nothing changes).
pub(crate) fn snap(
    s: &mut Session,
    note: vibenote_model::NoteId,
    stroke: u64,
) -> Result<Option<(ShapeKind, u64)>, EngineError> {
    if !matches!(super::pdf::object(s, note, stroke)?.kind, ObjectKind::Stroke(_)) {
        return Err(EngineError::Model(format!("object {stroke} is not a stroke")));
    }
    let Some((ops, kind, id)) = snap_ops(s, note, stroke, 0.0)? else {
        return Ok(None);
    };
    s.commit(Scope::Note(note), "snap shape", ops)?;
    Ok(Some((kind, id)))
}

/// The ops that replace stroke `stroke` by the clean shape it looks like, the shape's kind and
/// new id; `None` if it is not a stroke, does not look like a shape, or its box is smaller than
/// `min_side` (canvas units).
pub(crate) fn snap_ops(
    s: &mut Session,
    note: vibenote_model::NoteId,
    stroke: u64,
    min_side: f64,
) -> Result<Option<(Vec<Op>, ShapeKind, u64)>, EngineError> {
    let o = super::pdf::object(s, note, stroke)?;
    let ObjectKind::Stroke(st) = &o.kind else {
        return Ok(None);
    };
    let b = st.bounds();
    if b.width().max(b.height()) < min_side {
        return Ok(None);
    }
    let pts: Vec<Point> = st.samples.iter().map(|q| Point::new(q.x, q.y)).collect();
    let Some(found) = vibenote_ink::shapes::classify(&pts) else {
        return Ok(None);
    };
    let shape = CanvasObject {
        id: ObjectId(s.ws.alloc_id()?),
        z: o.z,
        kind: ObjectKind::Shape {
            shape: found.kind,
            points: found.points,
            closed: found.closed,
            color: st.color,
            width: st.width,
        },
    };
    let id = shape.id.0;
    Ok(Some((
        vec![
            Op::RemoveObject { note, object: o },
            Op::InsertObject { note, object: shape },
        ],
        found.kind,
        id,
    )))
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "shape.create",
            doc: "Create a shape: line/arrow (from → to), rectangle/ellipse (opposite corners), polygon/triangle (points); one undo step.",
            reqs: &["R-SHAPE-001", "R-CANVAS-003"],
            implemented: true,
        },
        |s: &mut Session, p: CreateP| {
            let note = super::ink::target_note(s, p.note)?;
            let two = || -> Result<Vec<Point>, EngineError> {
                match (p.from, p.to) {
                    (Some(a), Some(b)) => Ok(vec![pt(a), pt(b)]),
                    _ => Err(EngineError::Model("this shape needs `from` and `to`".into())),
                }
            };
            let many = |min: usize, exact: bool| -> Result<Vec<Point>, EngineError> {
                let v: Vec<Point> = p.points.clone().unwrap_or_default().into_iter().map(pt).collect();
                if v.len() < min || (exact && v.len() != min) {
                    return Err(EngineError::Model(format!("this shape needs {min}{} points", if exact { "" } else { "+" })));
                }
                Ok(v)
            };
            let (shape, points, closed) = match p.kind {
                KindP::Line => (ShapeKind::Line, two()?, false),
                KindP::Arrow => (ShapeKind::Arrow, two()?, false),
                KindP::Rectangle => (ShapeKind::Rectangle, two()?, true),
                KindP::Ellipse => (ShapeKind::Ellipse, two()?, true),
                KindP::Polygon => (ShapeKind::Polygon, many(3, false)?, true),
                KindP::Triangle => (ShapeKind::Triangle, many(3, true)?, true),
            };
            let id = create(
                s,
                note,
                shape,
                points,
                closed,
                p.color.map_or(Color::INK, Color),
                p.width.unwrap_or(2.0),
            )?;
            Ok(Created { id })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "shape.snap",
            doc: "Turn a held rough stroke (≥ 500 ms still) into the clean shape it looks like; strokes not held, or not recognised, stay ink. One undo step back to the ink.",
            reqs: &["R-SHAPE-002"],
            implemented: true,
        },
        |s: &mut Session, p: SnapP| {
            let note = super::ink::target_note(s, p.note)?;
            if p.held_ms < SNAP_HOLD_MS {
                return Ok(Snapped { kind: None, id: None });
            }
            Ok(match snap(s, note, p.stroke)? {
                Some((kind, id)) => Snapped {
                    kind: Some(kind),
                    id: Some(id),
                },
                None => Snapped { kind: None, id: None },
            })
        },
    )))?;
    for (id, doc, dir) in [
        (
            "ruler.rotate_left",
            "Rotate the ruler 15° counter-clockwise to the next multiple of 15° (Alt+Left).",
            -1.0,
        ),
        (
            "ruler.rotate_right",
            "Rotate the ruler 15° clockwise to the next multiple of 15° (Alt+Right).",
            1.0,
        ),
    ] {
        r.register(Box::new(Typed::new(
            Spec {
                id,
                doc,
                reqs: &["R-RULER-001"],
                implemented: true,
            },
            move |s: &mut Session, _: NoP| {
                let q = s.ruler.angle_deg / 15.0;
                let next = if dir > 0.0 {
                    (q + 1e-9).floor() + 1.0
                } else {
                    (q - 1e-9).ceil() - 1.0
                };
                s.ruler.angle_deg = (next * 15.0).rem_euclid(360.0);
                s.ruler.visible = true;
                Ok(s.ruler)
            },
        )))?;
    }
    r.register(Box::new(Typed::new(
        Spec {
            id: "ruler.toggle",
            doc: "Show/hide the ruler, move it (`at`, a point on its edge) and rotate it (`angle_deg`, `snap` to 15°). Strokes starting within 12 px of the edge snap to it.",
            reqs: &["R-RULER-001"],
            implemented: true,
        },
        |s: &mut Session, p: RulerP| {
            let mut r = s.ruler;
            r.visible = p.visible.unwrap_or(!r.visible);
            if let Some([x, y]) = p.at {
                let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
                if !(ok(x) && ok(y)) {
                    return Err(EngineError::Model("`at` out of range".into()));
                }
                (r.x, r.y) = (x, y);
            }
            if let Some(a) = p.angle_deg {
                if !a.is_finite() {
                    return Err(EngineError::Model("angle must be finite".into()));
                }
                r.angle_deg = a.rem_euclid(360.0);
            }
            if p.snap {
                r.angle_deg = ((r.angle_deg / 15.0).round() * 15.0).rem_euclid(360.0);
            }
            s.ruler = r;
            Ok(r)
        },
    )))?;
    Ok(())
}
