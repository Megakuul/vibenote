//! Lasso selection and editing the selection (R-PEN-009): select strokes mostly inside a lasso,
//! move/scale/rotate them, recolour, delete; copy, cut and paste them within and between notes
//! (R-UNDO-002, in-app clipboard). Every edit is one undo step in the note's scope.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::{Point, Rect};
use vibenote_model::{CanvasObject, Color, NoteId, ObjectId, ObjectKind, Op};

/// Share of a stroke's length that must be inside the lasso [SPEC proposal: ≥ 50 %].
const INSIDE: f64 = 0.5;
/// Scale factors accepted per transform.
const SCALE: (f64, f64) = (1.0e-3, 1.0e3);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Lasso {
    note: Option<u64>,
    /// Closed polygon, canvas units.
    path: Vec<[f64; 2]>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetP {
    note: Option<u64>,
    ids: Vec<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransformP {
    /// Canvas units.
    translate: Option<[f64; 2]>,
    /// Uniform scale (stroke widths scale too).
    scale: Option<f64>,
    rotate_deg: Option<f64>,
    /// Centre of scaling/rotation (default: the centre of the selection's bounds).
    pivot: Option<[f64; 2]>,
    /// Undo the last transform as a new step (exact while nothing else changed the objects).
    #[serde(default)]
    invert_last: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Recolor {
    /// 0xRRGGBBAA
    color: u32,
}

#[derive(Serialize)]
struct Selected {
    ids: Vec<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PasteP {
    /// Target note (default: the current note).
    note: Option<u64>,
    /// Canvas point for the top-left of the pasted content's bounds. Default: where it was
    /// copied from, or a bit further down-right for each paste back into its own note.
    at: Option<[f64; 2]>,
}

#[derive(Serialize)]
struct Copied {
    count: usize,
}

/// Offset of each paste back into the source note at the default position (canvas units).
const PASTE_STEP: f64 = 24.0;

/// The points that place an object (stroke samples, shape corners, other objects' origin).
fn anchors(o: &CanvasObject) -> Vec<Point> {
    match &o.kind {
        ObjectKind::Stroke(st) => st.samples.iter().map(vibenote_model::Sample::point).collect(),
        ObjectKind::Shape { points, .. } => points.clone(),
        ObjectKind::Text { origin, .. }
        | ObjectKind::Pdf { origin, .. }
        | ObjectKind::Image { origin, .. }
        | ObjectKind::Link { origin, .. }
        | ObjectKind::Audio { origin, .. } => {
            let mut v = vec![*origin];
            if let Some(b) = o.kind.bounds() {
                v.extend([b.min, b.max]);
            }
            v
        }
    }
}

fn bounds(objs: &[CanvasObject]) -> Rect {
    let mut r = Rect::EMPTY;
    for o in objs {
        for p in anchors(o) {
            r.include(p);
        }
    }
    r
}

/// Copies the selection to the clipboard (originals in (z, id) order).
fn copy(s: &mut Session) -> Result<(NoteId, Vec<CanvasObject>), EngineError> {
    let (note, mut objs) = selected(s)?;
    objs.sort_by_key(|o| (o.z, o.id));
    s.clipboard = crate::Clipboard {
        objects: objs.clone(),
        note: Some(note),
        pastes_home: 0,
    };
    Ok((note, objs))
}

/// Selects objects of `note` mostly inside `path` (strokes by length, others by area);
/// returns their ids (ascending).
pub(crate) fn lasso(s: &mut Session, note: NoteId, path: &[(f64, f64)]) -> Result<Vec<u64>, EngineError> {
    vibenote_ink::lasso::check(path).map_err(EngineError::Model)?;
    let mut area = Rect::EMPTY;
    for &(x, y) in path {
        area.include(Point::new(x, y));
    }
    let mut ids = s.candidates(note, &area)?;
    // Prettified strokes count where they are shown (ADR 0008 display transforms).
    let display = s.display_transforms(note);
    if !display.is_empty() {
        ids.extend(s.prettified_near(note, &display, &area));
        ids.sort_unstable();
        ids.dedup();
    }
    let canvas = s.ws.canvas(note)?;
    let mut picked = Vec::new();
    for id in ids {
        let Some(o) = canvas.objects.get(&id) else { continue };
        match &o.kind {
            ObjectKind::Stroke(st) => {
                let t = display.get(&id);
                let pts: Vec<(f64, f64)> = st
                    .samples
                    .iter()
                    .map(|p| t.map_or((p.x, p.y), |t| crate::hwr::map_point(t, (p.x, p.y))))
                    .collect();
                if vibenote_ink::lasso::inside_fraction(&pts, path) >= INSIDE {
                    picked.push(id.0);
                }
            }
            // Other objects: the share of their area inside, sampled on a grid (R-PEN-009).
            _ => {
                let Some(b) = s.object_box(o).filter(|b| !b.is_empty()) else {
                    continue;
                };
                let n = 5u32;
                let mut inside = 0u32;
                for i in 0..n {
                    for j in 0..n {
                        let fx = (f64::from(i) + 0.5) / f64::from(n);
                        let fy = (f64::from(j) + 0.5) / f64::from(n);
                        let q = (b.min.x + fx * b.width(), b.min.y + fy * b.height());
                        if vibenote_ink::lasso::contains(path, q) {
                            inside = inside.saturating_add(1);
                        }
                    }
                }
                if f64::from(inside) >= INSIDE * f64::from(n * n) {
                    picked.push(id.0);
                }
            }
        }
    }
    s.selection = crate::Selection {
        note: Some(note),
        ids: picked.clone(),
        last: None,
    };
    Ok(picked)
}

/// The selected objects that still exist (undo may have removed some).
fn selected(s: &Session) -> Result<(NoteId, Vec<CanvasObject>), EngineError> {
    let note = s
        .selection
        .note
        .ok_or_else(|| EngineError::Model("nothing is selected".into()))?;
    let canvas = s.ws.canvas(note)?;
    let objs: Vec<CanvasObject> = s
        .selection
        .ids
        .iter()
        .filter_map(|id| canvas.objects.get(&ObjectId(*id)).cloned())
        .collect();
    if objs.is_empty() {
        return Err(EngineError::Model("nothing is selected".into()));
    }
    Ok((note, objs))
}

fn finite(v: f64, what: &str, max: f64) -> Result<f64, EngineError> {
    if v.is_finite() && v.abs() <= max {
        Ok(v)
    } else {
        Err(EngineError::Model(format!("{what} out of range")))
    }
}

/// Applies `x' = pivot + translate + R·S·(x − pivot)` to every sample.
pub(crate) fn transformed(objs: &[CanvasObject], p: &TransformP) -> Result<Vec<CanvasObject>, EngineError> {
    let [dx, dy] = p.translate.unwrap_or([0.0, 0.0]);
    let (dx, dy) = (finite(dx, "translate", 1.0e9)?, finite(dy, "translate", 1.0e9)?);
    let k = p.scale.unwrap_or(1.0);
    if !(k.is_finite() && (SCALE.0..=SCALE.1).contains(&k)) {
        return Err(EngineError::Model("scale must be within 0.001..=1000".into()));
    }
    let a = finite(p.rotate_deg.unwrap_or(0.0), "rotate_deg", 1.0e6)?.to_radians();
    let (sin, cos) = a.sin_cos();
    let pivot = match p.pivot {
        Some([x, y]) => (finite(x, "pivot", 1.0e9)?, finite(y, "pivot", 1.0e9)?),
        None => {
            let r = bounds(objs);
            if r.is_empty() {
                (0.0, 0.0)
            } else {
                (f64::midpoint(r.min.x, r.max.x), f64::midpoint(r.min.y, r.max.y))
            }
        }
    };
    let rotates = a != 0.0;
    if rotates
        && objs
            .iter()
            .any(|o| !matches!(o.kind, ObjectKind::Stroke(_) | ObjectKind::Shape { .. }))
    {
        return Err(EngineError::Model("only ink and shapes can be rotated".into()));
    }
    let map = |q: Point| {
        let (x, y) = ((q.x - pivot.0) * k, (q.y - pivot.1) * k);
        Point::new(pivot.0 + dx + x * cos - y * sin, pivot.1 + dy + x * sin + y * cos)
    };
    #[allow(clippy::cast_possible_truncation)] // k ≤ 1000, the model checks the result
    let kf = k as f32;
    let mut out = Vec::with_capacity(objs.len());
    for o in objs {
        let mut o = o.clone();
        match &mut o.kind {
            ObjectKind::Stroke(st) => {
                for smp in &mut st.samples {
                    let q = map(smp.point());
                    (smp.x, smp.y) = (q.x, q.y);
                }
                st.width *= kf;
            }
            ObjectKind::Shape { points, width, .. } => {
                for q in points.iter_mut() {
                    *q = map(*q);
                }
                *width *= kf;
            }
            ObjectKind::Text {
                origin,
                width,
                size,
                marks,
                ..
            } => {
                *origin = map(*origin);
                *width *= k;
                *size *= kf;
                for m in marks.iter_mut() {
                    if let vibenote_model::TextStyle::Size { size } = &mut m.style {
                        *size *= kf;
                    }
                }
            }
            ObjectKind::Pdf { origin, scale, gap, .. } => {
                *origin = map(*origin);
                *scale *= k;
                *gap *= k;
            }
            ObjectKind::Image {
                origin, width, height, ..
            } => {
                *origin = map(*origin);
                *width *= k;
                *height *= k;
            }
            ObjectKind::Link { origin, .. } | ObjectKind::Audio { origin, .. } => *origin = map(*origin),
        }
        out.push(o);
    }
    Ok(out)
}

/// `selection.transform`: one undo step.
fn run_transform(s: &mut Session, p: TransformP) -> Result<(), EngineError> {
    let (note, objs) = selected(s)?;
    let after = if p.invert_last {
        let (before, after) = s
            .selection
            .last
            .clone()
            .ok_or_else(|| EngineError::Model("no transform to invert".into()))?;
        if after != objs {
            return Err(EngineError::Model(
                "the selection changed since the last transform".into(),
            ));
        }
        before
    } else {
        transformed(&objs, &p)?
    };
    let ops = after
        .into_iter()
        .map(|o| Op::ReplaceObject { note, after: o })
        .collect();
    s.commit(Scope::Note(note), "transform", ops)?;
    // What the model stored (samples are snapped to its grid).
    let (_, now) = selected(s)?;
    s.selection.last = if p.invert_last { None } else { Some((objs, now)) };
    Ok(())
}

/// The `selection.transform` parameters of a drag (`crate::gesture`).
pub(crate) fn xform_params(x: &crate::gesture::Xform) -> TransformP {
    TransformP {
        translate: Some([x.translate.0, x.translate.1]),
        scale: Some(x.scale),
        rotate_deg: Some(x.rotate_deg),
        pivot: Some([x.pivot.0, x.pivot.1]),
        invert_last: false,
    }
}

/// Commits a drag of the selection as one `selection.transform` step.
pub(crate) fn apply_xform(s: &mut Session, x: &crate::gesture::Xform) -> Result<(), EngineError> {
    run_transform(s, xform_params(x))
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.lasso",
            doc: "Select the strokes mostly (≥ 50 % of their length) inside a lasso polygon.",
            reqs: &["R-PEN-009"],
            implemented: true,
        },
        |s: &mut Session, p: Lasso| {
            let note = super::ink::target_note(s, p.note)?;
            let path: Vec<(f64, f64)> = p.path.iter().map(|&[x, y]| (x, y)).collect();
            Ok(Selected {
                ids: lasso(s, note, &path)?,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.set",
            doc: "Select objects by id.",
            reqs: &["R-PEN-009", "R-UNDO-002"],
            implemented: true,
        },
        |s: &mut Session, p: SetP| {
            let note = super::ink::target_note(s, p.note)?;
            if p.ids.len() > crate::MAX_IDS {
                return Err(EngineError::Model(format!(
                    "at most {} objects per command",
                    crate::MAX_IDS
                )));
            }
            // Ids stored but not loaded yet exist too (a note still loading).
            let all_here = {
                let c = s.ws.canvas(note)?;
                p.ids.iter().all(|id| c.objects.contains_key(&ObjectId(*id)))
            };
            if !all_here {
                s.ensure_complete(note)?;
            }
            let canvas = s.ws.canvas(note)?;
            if let Some(id) = p.ids.iter().find(|id| !canvas.objects.contains_key(&ObjectId(**id))) {
                return Err(EngineError::Model(format!("object {id} not found")));
            }
            let mut ids = p.ids;
            ids.sort_unstable();
            ids.dedup();
            s.selection = crate::Selection {
                note: Some(note),
                ids: ids.clone(),
                last: None,
            };
            Ok(Selected { ids })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.clear",
            doc: "Select nothing (and drop a polygon being placed with the shape tool).",
            reqs: &["R-PEN-009"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            s.selection = crate::Selection::default();
            s.live.polygon.clear(); // Escape also drops a polygon being placed
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.transform",
            doc: "Move, scale (widths too) and rotate the selection around a pivot (default its centre), one undo step; `invert_last` undoes the last transform as a new step.",
            reqs: &["R-PEN-009"],
            implemented: true,
        },
        run_transform,
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.recolor",
            doc: "Recolour the selected strokes (one undo step).",
            reqs: &["R-PEN-009"],
            implemented: true,
        },
        |s: &mut Session, p: Recolor| {
            let (note, objs) = selected(s)?;
            let ops = objs
                .into_iter()
                .map(|mut o| {
                    if let ObjectKind::Stroke(st) = &mut o.kind {
                        st.color = Color(p.color);
                    }
                    Op::ReplaceObject { note, after: o }
                })
                .collect();
            s.selection.last = None;
            s.commit(Scope::Note(note), "recolour", ops)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "clipboard.copy",
            doc: "Copy the selection to the clipboard.",
            reqs: &["R-UNDO-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            let (_, objs) = copy(s)?;
            Ok(Copied { count: objs.len() })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "clipboard.cut",
            doc: "Copy the selection to the clipboard and delete it (one undo step).",
            reqs: &["R-UNDO-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            let saved = s.clipboard.clone();
            let (note, objs) = copy(s)?;
            let count = objs.len();
            let ops = objs
                .into_iter()
                .map(|object| Op::RemoveObject { note, object })
                .collect();
            if let Err(e) = s.commit(Scope::Note(note), "cut", ops) {
                s.clipboard = saved; // a failed command changes nothing
                return Err(e);
            }
            s.selection = crate::Selection::default();
            Ok(Copied { count })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "clipboard.paste",
            doc: "Paste the clipboard as new objects on top (one undo step); the pasted objects become the selection.",
            reqs: &["R-UNDO-002"],
            implemented: true,
        },
        |s: &mut Session, p: PasteP| {
            let note = super::ink::target_note(s, p.note)?;
            if s.clipboard.objects.is_empty() {
                return Err(EngineError::Model("the clipboard is empty".into()));
            }
            let clip = s.clipboard.objects.clone();
            let b = bounds(&clip);
            let home = s.clipboard.note == Some(note);
            let (dx, dy) = match p.at {
                Some([x, y]) if !b.is_empty() => (finite(x, "at", 1.0e9)? - b.min.x, finite(y, "at", 1.0e9)? - b.min.y),
                Some([x, y]) => (finite(x, "at", 1.0e9)?, finite(y, "at", 1.0e9)?),
                None if home => {
                    let k = f64::from(s.clipboard.pastes_home.saturating_add(1));
                    (PASTE_STEP * k, PASTE_STEP * k)
                }
                None => (0.0, 0.0),
            };
            let moved = transformed(
                &clip,
                &TransformP {
                    translate: Some([dx, dy]),
                    scale: None,
                    rotate_deg: None,
                    pivot: Some([0.0, 0.0]),
                    invert_last: false,
                },
            )?;
            let mut z = s.next_z(note)?;
            let mut ops = Vec::with_capacity(moved.len());
            let mut ids = Vec::with_capacity(moved.len());
            for mut o in moved {
                o.id = ObjectId(s.ws.alloc_id()?);
                o.z = z;
                z = z
                    .checked_add(1)
                    .ok_or_else(|| EngineError::Model("z-order range exhausted".into()))?;
                ids.push(o.id.0);
                ops.push(Op::InsertObject { note, object: o });
            }
            s.commit(Scope::Note(note), "paste", ops)?;
            if home && p.at.is_none() {
                s.clipboard.pastes_home = s.clipboard.pastes_home.saturating_add(1);
            }
            s.selection = crate::Selection {
                note: Some(note),
                ids: ids.clone(),
                last: None,
            };
            Ok(Selected { ids })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.delete",
            doc: "Delete the selected objects (one undo step).",
            reqs: &["R-PEN-009"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            let (note, objs) = selected(s)?;
            let ops = objs
                .into_iter()
                .map(|object| Op::RemoveObject { note, object })
                .collect();
            s.commit(Scope::Note(note), "delete", ops)?;
            s.selection = crate::Selection::default();
            Ok(())
        },
    )))?;
    Ok(())
}
