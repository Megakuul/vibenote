//! Ink commands (R-PEN-*).

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_model::{CanvasObject, Color, NoteId, ObjectId, ObjectKind, Op, Sample, Stroke, Tool};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SampleP {
    x: f64,
    y: f64,
    #[serde(default = "one")]
    p: f32,
    #[serde(default)]
    tx: f32,
    #[serde(default)]
    ty: f32,
    #[serde(default)]
    t_us: u64,
}
fn one() -> f32 {
    1.0
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ToolP {
    Pen,
    Highlighter,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StrokeAdd {
    /// Target note; defaults to the current note.
    note: Option<u64>,
    #[serde(default = "pen")]
    tool: ToolP,
    color: Option<u32>,
    #[serde(default = "two")]
    width: f32,
    samples: Vec<SampleP>,
    /// When the stroke began (pen-event clock, CLOCK_MONOTONIC µs; default: now). Links it to
    /// a running recording (R-AUDIO-002).
    written_at_us: Option<u64>,
}
fn pen() -> ToolP {
    ToolP::Pen
}
fn two() -> f32 {
    2.0
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObjectsDelete {
    note: Option<u64>,
    ids: Vec<u64>,
}

#[derive(Serialize)]
struct Created {
    id: u64,
}

pub(crate) fn target_note(s: &Session, note: Option<u64>) -> Result<NoteId, EngineError> {
    let n = note
        .map(NoteId)
        .or(s.current_note)
        .ok_or_else(|| EngineError::Model("no note given and no current note".into()))?;
    s.ws.note(n)?;
    Ok(n)
}

/// Commits a stroke as one undo step in the note's scope; returns its id.
/// Adds a stroke (one undo step). `written_at_us` is when it began (pen-event clock): written
/// during a recording, it is linked to that moment in the same step (R-AUDIO-002).
pub(crate) fn add_stroke(
    s: &mut Session,
    note: NoteId,
    mut stroke: Stroke,
    written_at_us: Option<u64>,
) -> Result<ObjectId, EngineError> {
    super::shape::ruler_snap(s, &mut stroke);
    let z = s.next_z(note)?;
    let id = ObjectId(s.ws.alloc_id()?);
    let object = CanvasObject {
        id,
        z,
        kind: ObjectKind::Stroke(stroke),
    };
    let mut ops = vec![Op::InsertObject { note, object }];
    if let Some(mark) = s.audio_mark_for(written_at_us) {
        ops.push(Op::SetAudioMark {
            note,
            stroke: id,
            after: Some(mark),
        });
    }
    s.commit(Scope::Note(note), "stroke", ops)?;
    Ok(id)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Erase {
    /// Target note; defaults to the current note.
    note: Option<u64>,
    /// Eraser path, canvas units.
    path: Vec<[f64; 2]>,
    /// Eraser radius in canvas units (default 4).
    radius: Option<f64>,
}

/// Stroke or area eraser (R-PEN-006..008).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EraseMode {
    /// Whole strokes the eraser touches.
    Stroke,
    /// Only the ink under the eraser; strokes are split into the remaining pieces.
    Area,
}

#[derive(Serialize, Default)]
pub(crate) struct Erased {
    /// Strokes removed (whole, or replaced by pieces).
    pub erased: Vec<u64>,
    /// Pieces created by the area eraser.
    pub created: Vec<u64>,
}

/// The eraser path in windows of at most `MAX_ERASER_POINTS` points, each overlapping the
/// previous by one point, so any path length erases exactly what the whole path would.
fn windows(path: &[(f64, f64)]) -> Vec<&[(f64, f64)]> {
    use vibenote_ink::erase::MAX_ERASER_POINTS;
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rest) = path.get(i..).filter(|r| !r.is_empty()) {
        let n = rest.len().min(MAX_ERASER_POINTS);
        out.extend(rest.get(..n));
        if n == rest.len() {
            break;
        }
        i = i.saturating_add(n.saturating_sub(1).max(1));
    }
    out
}

/// A shape's outline as ink strokes that look the same (pen, full pressure = the shape's width;
/// hand-drawn shapes as drawn, R-SHAPE-003); `None` for other objects.
fn shape_ink(s: &Session, kind: &ObjectKind) -> Option<Vec<Stroke>> {
    let ObjectKind::Shape {
        shape,
        points,
        closed,
        color,
        width,
    } = kind
    else {
        return None;
    };
    Some(s.shape_strokes(*shape, points, *closed, *color, *width))
}

/// Erases along `path` in one undo step (no step if nothing is hit). Paths from the pen may be
/// any length (windows); the command form caps them (`erase_cmd`).
pub(crate) fn erase(
    s: &mut Session,
    note: NoteId,
    path: &[(f64, f64)],
    radius: f64,
    mode: EraseMode,
) -> Result<Erased, EngineError> {
    use vibenote_ink::erase::{Eraser, split, touches};
    if path.len() > vibenote_model::limits::MAX_SAMPLES_PER_STROKE {
        return Err(EngineError::Model("eraser path too long".into()));
    }
    let erasers: Vec<Eraser<'_>> = windows(path).into_iter().map(|w| Eraser { path: w, radius }).collect();
    if erasers.is_empty() {
        return Err(EngineError::Model("eraser path must have at least 1 point".into()));
    }
    for e in &erasers {
        e.check().map_err(EngineError::Model)?;
    }
    let mut ids = s.candidates_along(note, path, radius)?;
    // Prettified strokes are hit where they are shown (ADR 0008 display transforms): the path
    // is mapped into each such stroke's own coordinates (the transforms are rigid, so the
    // radius stays the same).
    let display = s.display_transforms(note);
    if !display.is_empty() {
        let mut area = vibenote_geom::Rect::EMPTY;
        for &(x, y) in path {
            area.include(vibenote_geom::Point::new(x, y));
        }
        ids.extend(s.prettified_near(note, &display, &area.inflate(radius)));
        ids.sort_unstable();
        ids.dedup();
    }
    // Decide on borrowed strokes; clone only what is hit.
    let mut hits: Vec<(ObjectId, Option<Vec<Vec<vibenote_model::Sample>>>)> = Vec::new();
    {
        let canvas = s.ws.canvas(note)?;
        for id in ids {
            let Some(o) = canvas.objects.get(&id) else { continue };
            // Shapes (drawn or snapped from ink, R-SHAPE-002) erase like ink (owner
            // 2026-10-19): the stroke eraser removes them, the area eraser turns their outline
            // into ink and cuts it.
            if let Some(lines) = shape_ink(s, &o.kind) {
                match mode {
                    EraseMode::Stroke => {
                        if lines.iter().any(|l| erasers.iter().any(|e| touches(l, e))) {
                            hits.push((id, None));
                        }
                    }
                    EraseMode::Area => {
                        let mut cut = false;
                        let mut pieces = Vec::new();
                        for l in lines {
                            let mut ps = vec![l.samples.clone()];
                            for e in &erasers {
                                let mut next = Vec::with_capacity(ps.len());
                                for samples in ps.drain(..) {
                                    let piece = Stroke {
                                        samples,
                                        ..l.clone_header()
                                    };
                                    match split(&piece, e) {
                                        Some(c) => {
                                            cut = true;
                                            next.extend(c);
                                        }
                                        None => next.push(piece.samples),
                                    }
                                }
                                ps = next;
                            }
                            pieces.extend(ps);
                        }
                        if cut {
                            hits.push((id, Some(pieces)));
                        }
                    }
                }
                continue;
            }
            let ObjectKind::Stroke(st) = &o.kind else { continue };
            let local: Option<Vec<(f64, f64)>> = display
                .get(&id)
                .and_then(crate::hwr::invert)
                .map(|inv| path.iter().map(|p| crate::hwr::map_point(&inv, *p)).collect());
            let local_erasers: Vec<Eraser<'_>>;
            let erasers: &[Eraser<'_>] = match &local {
                Some(lp) => {
                    local_erasers = windows(lp).into_iter().map(|w| Eraser { path: w, radius }).collect();
                    &local_erasers
                }
                None => &erasers,
            };
            match mode {
                EraseMode::Stroke => {
                    if erasers.iter().any(|e| touches(st, e)) {
                        hits.push((id, None));
                    }
                }
                EraseMode::Area => {
                    // Each window cuts the pieces the previous ones left.
                    let mut pieces: Option<Vec<Vec<vibenote_model::Sample>>> = None;
                    for e in erasers {
                        match &mut pieces {
                            None => pieces = split(st, e),
                            Some(ps) => {
                                let mut next = Vec::with_capacity(ps.len());
                                for samples in ps.drain(..) {
                                    let piece = Stroke {
                                        samples,
                                        ..st.clone_header()
                                    };
                                    match split(&piece, e) {
                                        Some(cut) => next.extend(cut),
                                        None => next.push(piece.samples),
                                    }
                                }
                                *ps = next;
                            }
                        }
                    }
                    if let Some(ps) = pieces {
                        hits.push((id, Some(ps)));
                    }
                }
            }
        }
    }
    let mut out = Erased::default();
    let mut ops = Vec::new();
    // Prettify parameters of blocks whose strokes are erased or split: pieces keep their
    // stroke's display transform, erased strokes leave the block (same undo step).
    let mut blocks: std::collections::BTreeMap<ObjectId, vibenote_model::Prettify> = std::collections::BTreeMap::new();
    let block_of = |s: &Session, id: ObjectId| {
        s.ws.prettified.get(&note).and_then(|m| {
            m.iter()
                .find(|(_, p)| p.strokes.contains(&id))
                .map(|(b, p)| (*b, p.clone()))
        })
    };
    for (id, pieces) in hits {
        let Some(o) = s.ws.canvas(note)?.objects.get(&id).cloned() else {
            continue;
        };
        let template = match &o.kind {
            ObjectKind::Stroke(st) => st.clone_header(),
            k => match shape_ink(s, k).and_then(|l| l.into_iter().next()) {
                Some(l) => l.clone_header(),
                None => continue,
            },
        };
        let z = o.z;
        out.erased.push(id.0);
        ops.push(Op::RemoveObject { note, object: o });
        let transform = display.get(&id).copied();
        if transform.is_some()
            && let Some((b, p)) = block_of(s, id)
        {
            blocks.entry(b).or_insert(p);
        }
        let mut piece_ids = Vec::new();
        for samples in pieces.unwrap_or_default() {
            let pid = ObjectId(s.ws.alloc_id()?);
            out.created.push(pid.0);
            piece_ids.push(pid);
            if let Some(t) = transform
                && let Some(p) = blocks.values_mut().find(|p| p.strokes.contains(&id))
            {
                p.strokes.push(pid);
                p.transforms.push(t);
            }
            ops.push(Op::InsertObject {
                note,
                object: CanvasObject {
                    id: pid,
                    z,
                    kind: ObjectKind::Stroke(Stroke {
                        samples,
                        ..template.clone()
                    }),
                },
            });
        }
        ops.extend(super::audio::inherit_marks(s, note, id, &piece_ids));
    }
    for (block, mut p) in blocks {
        let gone: std::collections::HashSet<u64> = out.erased.iter().copied().collect();
        let (strokes, transforms): (Vec<ObjectId>, Vec<[f64; 6]>) = p
            .strokes
            .iter()
            .zip(&p.transforms)
            .filter(|(id, _)| !gone.contains(&id.0))
            .map(|(id, t)| (*id, *t))
            .unzip();
        p.strokes = strokes;
        p.transforms = transforms;
        let after = if p.strokes.is_empty() { None } else { Some(p) };
        ops.push(Op::SetPrettify { note, block, after });
    }
    if !ops.is_empty() {
        s.commit(Scope::Note(note), "erase", ops)?;
    }
    Ok(out)
}

fn erase_cmd(s: &mut Session, p: Erase, mode: EraseMode) -> Result<Erased, EngineError> {
    let note = target_note(s, p.note)?;
    let path: Vec<(f64, f64)> = p.path.iter().map(|&[x, y]| (x, y)).collect();
    // Commands (control channel: hostile input) keep the per-path cap.
    let radius = p.radius.unwrap_or(4.0);
    vibenote_ink::erase::Eraser { path: &path, radius }
        .check()
        .map_err(EngineError::Model)?;
    erase(s, note, &path, radius, mode)
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "ink.erase_stroke",
            doc: "Erase whole strokes touched by an eraser path (one undo step).",
            reqs: &["R-PEN-006", "R-PEN-007"],
            implemented: true,
        },
        |s: &mut Session, p: Erase| erase_cmd(s, p, EraseMode::Stroke),
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "ink.erase_area",
            doc: "Erase the ink under an eraser path; strokes are split into the remaining pieces, which keep their samples (one undo step).",
            reqs: &["R-PEN-006", "R-PEN-008"],
            implemented: true,
        },
        |s: &mut Session, p: Erase| erase_cmd(s, p, EraseMode::Area),
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "ink.stroke.add",
            doc: "Add a pen/highlighter stroke with all samples (position, pressure, tilt, time) kept exactly.",
            reqs: &["R-PEN-002", "R-PEN-003", "R-PEN-004", "R-PEN-005"],
            implemented: true,
        },
        |s: &mut Session, p: StrokeAdd| {
            let note = target_note(s, p.note)?;
            let tool = match p.tool {
                ToolP::Pen => Tool::Pen,
                ToolP::Highlighter => Tool::Highlighter,
            };
            let samples = p
                .samples
                .into_iter()
                .map(|q| Sample {
                    x: q.x,
                    y: q.y,
                    pressure: q.p,
                    tilt_x: q.tx,
                    tilt_y: q.ty,
                    t_us: q.t_us,
                })
                .collect();
            let color = p.color.map(Color).unwrap_or(Color::INK);
            let id = add_stroke(
                s,
                note,
                Stroke {
                    tool,
                    color,
                    width: p.width,
                    samples,
                },
                p.written_at_us,
            )?;
            Ok(Created { id: id.0 })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "object.delete",
            doc: "Delete canvas objects by id (one undo step).",
            reqs: &["R-CANVAS-003", "R-PEN-009"],
            implemented: true,
        },
        |s: &mut Session, p: ObjectsDelete| {
            let note = target_note(s, p.note)?;
            if p.ids.len() > crate::MAX_IDS {
                return Err(EngineError::Model(format!(
                    "at most {} objects per command",
                    crate::MAX_IDS
                )));
            }
            let all_here = {
                let c = s.ws.canvas(note)?;
                p.ids.iter().all(|id| c.objects.contains_key(&ObjectId(*id)))
            };
            if !all_here {
                s.ensure_complete(note)?;
            }
            let c = s.ws.canvas(note)?;
            let mut ops = Vec::with_capacity(p.ids.len());
            for id in p.ids {
                let o = c
                    .objects
                    .get(&ObjectId(id))
                    .ok_or_else(|| EngineError::Model(format!("object {id} not found")))?;
                ops.push(Op::RemoveObject {
                    note,
                    object: o.clone(),
                });
            }
            s.commit(Scope::Note(note), "delete", ops)
        },
    )))?;
    Ok(())
}
