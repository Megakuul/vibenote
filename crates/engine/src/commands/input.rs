//! Pen input state machine (`input.pen`): raw tablet frames → live stroke → committed stroke.
//! Mouse input (`input.pointer`, R-PEN-012) drives the same machine with fixed pressure 0.5;
//! the middle button pans. Deterministic: depends only on the events (incl. their timestamps)
//! and the session.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, LiveStroke, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use vibenote_ink::input::{PenEvent, PenEventKind, PenTool};
use vibenote_model::Sample;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PenBatch {
    /// Tablet frames in surface coordinates (see ink fixture format v1).
    #[schemars(with = "Vec<Value>")]
    events: Vec<PenEvent>,
}

#[derive(Serialize, Default)]
struct BatchResult {
    /// Ids of strokes committed by this batch.
    committed: Vec<u64>,
    /// Per-stroke errors (e.g. eraser not implemented yet). The batch itself still succeeds.
    errors: Vec<String>,
}

fn sample(s: &Session, e: &PenEvent, t0: u64) -> Sample {
    let (x, y) = s.view.to_canvas(e.x, e.y);
    Sample {
        x,
        y,
        pressure: e.p,
        tilt_x: e.tx,
        tilt_y: e.ty,
        t_us: e.t_us.saturating_sub(t0),
    }
}

use crate::{BTN_STYLUS, BTN_STYLUS2, ERASER_RADIUS_PX};

/// At most `max` points of `path`, evenly spaced by index, first and last kept (a long lasso
/// is a polygon; its shape survives this).
fn thin(path: Vec<(f64, f64)>, max: usize) -> Vec<(f64, f64)> {
    let n = path.len();
    if n <= max || max < 2 {
        return path;
    }
    let last = n.saturating_sub(1);
    let steps = max.saturating_sub(1);
    (0..max)
        .filter_map(|k| {
            let i = k.saturating_mul(last).checked_div(steps)?;
            path.get(i).copied()
        })
        .collect()
}

fn finish(s: &mut Session, out: &mut BatchResult) {
    use super::ink::{EraseMode, erase};
    use vibenote_model::settings::EraserMode;
    let Some(ls) = s.live.stroke.take() else { return };
    let path: Vec<(f64, f64)> = ls.samples.iter().map(|p| (p.x, p.y)).collect();
    // The lasso: a lasso barrel button, or the lasso tool with the pen tip (R-PEN-009/010).
    if s.lassoing(&ls) {
        // A tap selects the topmost object under it (any kind: ink, text, images, PDFs).
        let zoom = s.zoom();
        if let Some(&(x0, y0)) = path.first()
            && path.iter().all(|&(x, y)| (x - x0).hypot(y - y0) * zoom < LASSO_TAP_PX)
        {
            match super::pdf::hits_at(s, ls.note, (x0, y0), LASSO_TAP_PX / zoom) {
                Ok(hits) => {
                    s.selection = crate::Selection {
                        note: Some(ls.note),
                        ids: hits.into_iter().take(1).collect(),
                        last: None,
                    };
                }
                Err(e) => out.errors.push(e.to_string()),
            }
            return;
        }
        let path = thin(path, vibenote_ink::lasso::MAX_LASSO_POINTS);
        if path.len() >= 3
            && let Err(e) = super::selection::lasso(s, ls.note, &path)
        {
            out.errors.push(e.to_string());
        }
        return;
    }
    // The eraser end and eraser barrel buttons erase with the eraser mode and leave the
    // selected tool alone (R-PEN-006/010); the eraser tools erase with the pen tip.
    let mode = s.erasing(&ls).map(|m| match m {
        EraserMode::Stroke => EraseMode::Stroke,
        EraserMode::Area => EraseMode::Area,
    });
    if let Some(mode) = mode {
        let zoom = if s.view.zoom.is_finite() && s.view.zoom > 0.0 {
            s.view.zoom
        } else {
            1.0
        };
        if let Err(e) = erase(s, ls.note, &path, ERASER_RADIUS_PX / zoom, mode) {
            out.errors.push(e.to_string());
        }
        return;
    }
    // A tap on a cover reveals or hides what is under it; on an audio player it plays,
    // pauses or seeks (R-PEN-014, R-AUDIO-004). Nothing is drawn.
    if let Some(&(x0, y0)) = path.first()
        && path.iter().all(|&(x, y)| (x - x0).hypot(y - y0) * s.zoom() < TAP_PX)
    {
        match tap_object(s, ls.note, (x0, y0), ls.mouse) {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                out.errors.push(e.to_string());
                return;
            }
        }
    }
    // The text tool: a tap edits the text box there or starts a new one (R-TEXT-001).
    if s.settings.tool == vibenote_model::settings::ToolKind::Text {
        if let Some(first) = ls.samples.first()
            && let Err(e) = super::text::tap(s, ls.note, (first.x, first.y))
        {
            out.errors.push(e.to_string());
        }
        return;
    }
    if s.settings.tool == vibenote_model::settings::ToolKind::Shape {
        if let Err(e) = shape_from_drag(s, &ls) {
            out.errors.push(e.to_string());
        }
        return;
    }
    let end_us = ls.t0_us.saturating_add(ls.samples.last().map_or(0, |p| p.t_us));
    match crate::tool_stroke(&s.settings, ls.samples) {
        Some(stroke) => match super::ink::add_stroke(s, ls.note, stroke, Some(ls.t0_us)) {
            Ok(id) if lift_snap(s, ls.note, id.0, ls.tool, ls.button) => out.committed.push(id.0),
            Ok(id) => {
                out.committed.push(id.0);
                // Writing blocks for recognition (R-HWR-001, R-HWR-008); covers are never
                // writing (R-PEN-014).
                if s.settings.tool != vibenote_model::settings::ToolKind::Cover {
                    s.block_add(ls.note, id, ls.t0_us, end_us);
                }
            }
            Err(e) => out.errors.push(e.to_string()),
        },
        // Tools without ink yet (lasso, shapes, text) arrive with their milestones.
        None => out
            .errors
            .push(EngineError::NotImplemented(format!("pen input with the {:?} tool", s.settings.tool)).to_string()),
    }
}

/// With `snap_on_lift`, a pen stroke this large (screen px) that looks like a shape becomes one
/// when the pen lifts; smaller strokes are left as ink (letters such as "o" or "l").
const LIFT_SNAP_MIN_PX: f64 = 64.0;

/// The owner's opt-in (`settings.snap_on_lift`): snaps a just-committed pen stroke that looks
/// like a shape (its own undo step back to the ink). Returns true if it snapped.
fn lift_snap(s: &mut Session, note: vibenote_model::NoteId, id: u64, tool: PenTool, button: Option<u32>) -> bool {
    use vibenote_model::settings::ToolKind;
    if !s.settings.snap_on_lift
        || s.settings.tool != ToolKind::Pen
        || tool != PenTool::Pen
        || s.barrel(button).is_some()
    {
        return false;
    }
    let min = LIFT_SNAP_MIN_PX / s.zoom();
    let Ok(Some((ops, _, _))) = super::shape::snap_ops(s, note, id, min) else {
        return false;
    };
    s.commit(crate::Scope::Note(note), "snap shape", ops).is_ok()
}

/// Drags shorter than this (screen px) are taps.
const SHAPE_TAP_PX: f64 = 3.0;
/// A polygon corner this close (screen px) to the first one closes it, to the last one ends it.
const POLYGON_CLOSE_PX: f64 = 10.0;

/// The shape the shape tool makes from a drag from `a` to `b` (canvas): line and arrow from
/// `a` to `b`, the others inside the box they span. `None` for polygons (placed by taps).
pub(crate) fn drag_shape(
    kind: vibenote_model::ShapeKind,
    a: (f64, f64),
    b: (f64, f64),
) -> Option<(Vec<vibenote_geom::Point>, bool)> {
    use vibenote_geom::Point;
    use vibenote_model::ShapeKind as K;
    let (pa, pb) = (Point::new(a.0, a.1), Point::new(b.0, b.1));
    Some(match kind {
        K::Line | K::Arrow => (vec![pa, pb], false),
        K::Rectangle | K::Ellipse => (vec![pa, pb], true),
        K::Triangle => {
            let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
            let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
            (
                vec![
                    Point::new(f64::midpoint(x0, x1), y0),
                    Point::new(x1, y1),
                    Point::new(x0, y1),
                ],
                true,
            )
        }
        K::Polygon => return None,
    })
}

/// Pen-up with the shape tool (R-SHAPE-001): a drag draws the selected shape; polygons get one
/// corner per tap and are finished by tapping the first corner (closed) or the last one again.
fn shape_from_drag(s: &mut Session, ls: &LiveStroke) -> Result<(), EngineError> {
    let (Some(first), Some(last)) = (ls.samples.first(), ls.samples.last()) else {
        return Ok(());
    };
    let zoom = s.zoom();
    let (a, b) = ((first.x, first.y), (last.x, last.y));
    let screen = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).hypot(p.1 - q.1) * zoom;
    let kind = s.settings.shape;
    let width = crate::pen_stroke(Vec::new()).width;
    if let Some((points, closed)) = drag_shape(kind, a, b) {
        if screen(a, b) < SHAPE_TAP_PX {
            return Ok(());
        }
        super::shape::create(s, ls.note, kind, points, closed, vibenote_model::Color::INK, width)?;
        return Ok(());
    }
    // Polygon: a drag places its start and end, a tap one corner.
    let mut corners = std::mem::take(&mut s.live.polygon);
    if corners.is_empty() && screen(a, b) >= SHAPE_TAP_PX {
        corners.push(a);
    }
    let done = corners.len() >= 3
        && (corners.first().is_some_and(|f| screen(*f, b) < POLYGON_CLOSE_PX)
            || corners.last().is_some_and(|l| screen(*l, b) < POLYGON_CLOSE_PX));
    if done {
        let points = corners.iter().map(|&(x, y)| vibenote_geom::Point::new(x, y)).collect();
        super::shape::create(s, ls.note, kind, points, true, vibenote_model::Color::INK, width)?;
        return Ok(());
    }
    if corners.last().is_none_or(|l| screen(*l, b) >= POLYGON_CLOSE_PX)
        && corners.len() < vibenote_model::limits::MAX_SAMPLES_PER_STROKE
    {
        corners.push(b);
    }
    s.live.polygon = corners;
    Ok(())
}

/// How still the pen must be to hold (screen px around its latest position).
const HOLD_STILL_PX: f64 = 3.0;
/// Rough shapes smaller than this (screen px across) never snap: letters like "o" stay ink.
const SNAP_MIN_PX: f64 = 32.0;

/// Holding the pen still for 500 ms at the end of a stroke turns it into the clean shape it
/// looks like (R-SHAPE-002): the stroke is committed as ink, then replaced by the shape in a
/// second undo step (undo returns to the ink). The rest of the touch draws nothing. `now_us`
/// is on the clock of the pen events. Returns true if it snapped.
pub(crate) fn hold(s: &mut Session, now_us: u64, out: &mut Vec<String>) -> bool {
    use vibenote_model::settings::ToolKind;
    let Some(ls) = s.live.stroke.as_ref() else { return false };
    let inks = matches!(s.settings.tool, ToolKind::Pen | ToolKind::Highlighter);
    if !inks || ls.tool != PenTool::Pen || s.barrel(ls.button).is_some() || ls.samples.len() < 3 {
        return false;
    }
    let zoom = s.zoom();
    let Some(last) = ls.samples.last() else { return false };
    // When the pen came to rest: the first of the trailing samples within the still radius.
    let still = HOLD_STILL_PX / zoom;
    let mut since = last.t_us;
    for p in ls.samples.iter().rev() {
        if (p.x - last.x).hypot(p.y - last.y) > still {
            break;
        }
        since = p.t_us;
    }
    let held_us = now_us.saturating_sub(ls.t0_us.saturating_add(since));
    if held_us < super::shape::SNAP_HOLD_MS.saturating_mul(1000) {
        return false;
    }
    let mut b = vibenote_geom::Rect::EMPTY;
    for p in &ls.samples {
        b.include(vibenote_geom::Point::new(p.x, p.y));
    }
    if b.width().max(b.height()) * zoom < SNAP_MIN_PX {
        return false;
    }
    let pts: Vec<vibenote_geom::Point> = ls.samples.iter().map(|p| vibenote_geom::Point::new(p.x, p.y)).collect();
    if vibenote_ink::shapes::classify(&pts).is_none() {
        return false;
    }
    let Some(ls) = s.live.stroke.take() else { return false };
    let mouse = ls.mouse;
    let Some(stroke) = crate::tool_stroke(&s.settings, ls.samples) else {
        return false;
    };
    let id = match super::ink::add_stroke(s, ls.note, stroke, Some(ls.t0_us)) {
        Ok(id) => id,
        Err(e) => {
            out.push(e.to_string());
            return false;
        }
    };
    s.live.snapped = Some(mouse);
    match super::shape::snap(s, ls.note, id.0) {
        Ok(r) => r.is_some(),
        Err(e) => {
            out.push(e.to_string());
            false
        }
    }
}

/// Moves the content by a surface offset (px).
fn pan_view(s: &mut Session, dx: f64, dy: f64) {
    let v = s.view;
    s.view = crate::View {
        origin_x: v.origin_x - dx / v.zoom,
        origin_y: v.origin_y - dy / v.zoom,
        ..v
    };
}

/// Starts a live stroke (ending any previous one).
fn begin(s: &mut Session, e: &PenEvent, button: Option<u32>, mouse: bool, out: &mut BatchResult) {
    finish(s, out);
    match s.current_note {
        Some(note) => {
            let first = sample(s, e, e.t_us);
            s.live.stroke = Some(LiveStroke {
                note,
                tool: e.tool,
                t0_us: e.t_us,
                samples: vec![first],
                button,
                mouse,
            });
        }
        None => out.errors.push("pen down without a current note".into()),
    }
}

/// Ends a drag of the selection: a real drag is one `selection.transform` step, a tap
/// changes nothing (R-PEN-009).
fn finish_drag(s: &mut Session, out: &mut BatchResult) {
    let Some(d) = s.live.drag.take() else { return };
    let r = match (d.moved, d.text_width) {
        // A text box's width handle: its wrap width.
        (true, Some((w0, w))) if w != w0 => match s.single_text() {
            Some((id, _)) => super::text::set_width(s, d.note, id, w),
            None => Ok(()),
        },
        (true, _) if !d.xform.is_identity() => super::selection::apply_xform(s, &d.xform),
        // A tap on a selected text box with the text tool: type into it there.
        (false, _) if s.settings.tool == vibenote_model::settings::ToolKind::Text => match s.single_text() {
            Some((id, _)) => {
                let word = s.live.double;
                super::text::edit(s, d.note, id, d.start, word)
            }
            None => Ok(()),
        },
        _ => Ok(()),
    };
    if let Err(e) = r {
        out.errors.push(e.to_string());
    }
}

/// A text-tool press: true if it landed on a text box (typing there, or moving its frame).
fn text_press(s: &mut Session, e: &PenEvent, mouse: bool, out: &mut BatchResult) -> bool {
    let Some(note) = s.current_note else { return false };
    finish(s, out);
    let at = s.view.to_canvas(e.x, e.y);
    match super::text::press(s, note, at, s.live.shift, mouse) {
        Ok(super::text::Press::Missed) => false,
        Ok(_) => true,
        Err(err) => {
            out.errors.push(err.to_string());
            true
        }
    }
}

/// Strokes shorter than this (screen px) are taps (on covers, players).
const TAP_PX: f64 = 4.0;

/// A tap at `at` with a drawing tool: toggles the topmost cover there or controls an audio
/// player. True if it hit one (and nothing is drawn).
fn tap_object(s: &mut Session, note: vibenote_model::NoteId, at: (f64, f64), mouse: bool) -> Result<bool, EngineError> {
    use vibenote_model::{ObjectId, ObjectKind, Tool};
    let hits = super::pdf::hits_at(s, note, at, TAP_PX / s.zoom())?;
    let canvas = s.ws.canvas(note)?;
    for id in hits {
        let Some(o) = canvas.objects.get(&ObjectId(id)) else {
            continue;
        };
        match &o.kind {
            ObjectKind::Stroke(st) if st.tool == Tool::Cover => {
                let id = ObjectId(id);
                if !s.revealed.remove(&id) {
                    s.revealed.insert(id);
                }
                return Ok(true);
            }
            ObjectKind::Audio { .. } => {
                super::audio::tap_player(s, note, id, at)?;
                return Ok(true);
            }
            // A mouse click with the pen or highlighter types into a text box (R-TEXT-005).
            ObjectKind::Text { .. }
                if mouse
                    && matches!(
                        s.settings.tool,
                        vibenote_model::settings::ToolKind::Pen | vibenote_model::settings::ToolKind::Highlighter
                    ) =>
            {
                super::text::press(s, note, at, false, true)?;
                s.live.text_drag = None;
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Lasso taps shorter than this (screen px) select the object under them instead.
const LASSO_TAP_PX: f64 = 4.0;

/// A press: grabs the selection when it lands on its box or a handle (pen tip or left mouse
/// button, no barrel button held); otherwise a press elsewhere drops the selection. Returns
/// true if the press started a drag.
fn press_selection(s: &mut Session, e: &PenEvent, mouse: bool) -> bool {
    let tip = e.tool == PenTool::Pen && (mouse || s.barrel(s.live.button).is_none());
    let at = s.view.to_canvas(e.x, e.y);
    if tip && s.begin_drag(at, mouse) {
        return true;
    }
    if s.selection.note.is_some() && s.selection.note == s.current_note {
        s.selection = crate::Selection::default();
    }
    false
}

/// Feeds one event from the pen (`mouse` false) or the mouse (true). Each device only extends
/// and ends its own live stroke (review M4 M4: a touchpad click must not merge into pen ink).
fn step(s: &mut Session, e: &PenEvent, mouse: bool, out: &mut BatchResult) {
    if e.kind == PenEventKind::Down {
        let near = s.live.last_down.is_some_and(|(t, x, y)| {
            e.t_us.saturating_sub(t) <= DOUBLE_CLICK_US && (e.x - x).hypot(e.y - y) <= DOUBLE_CLICK_PX
        });
        // 1, 2 (double click), 3 (triple click), then a fourth press starts over.
        s.live.clicks = if near && s.live.clicks < 3 {
            s.live.clicks.saturating_add(1)
        } else {
            1
        };
        s.live.double = s.live.clicks == 2;
        s.live.last_down = Some((e.t_us, e.x, e.y));
    }
    let mine = |s: &Session| s.live.stroke.as_ref().is_some_and(|l| l.mouse == mouse);
    let dragging = s.live.drag.as_ref().is_some_and(|d| d.mouse == mouse);
    // After a hold turned the stroke into a shape, the touch ends without drawing.
    if s.live.snapped == Some(mouse) {
        match e.kind {
            PenEventKind::Up | PenEventKind::ProximityOut => s.live.snapped = None,
            PenEventKind::Motion => s.live.hover = Some(s.view.to_canvas(e.x, e.y)),
            _ => {}
        }
        if e.kind != PenEventKind::ProximityOut {
            return;
        }
    }
    match e.kind {
        PenEventKind::ProximityIn => s.live.tool = Some(e.tool),
        PenEventKind::ProximityOut => {
            if mine(s) {
                finish(s, out); // a lost `up` must not lose ink
            }
            if dragging {
                finish_drag(s, out);
            }
            s.live.tool = None;
            s.live.hover = None;
            s.live.button = None;
            if s.live.ruler_drag.is_some_and(|d| d.mouse == mouse) {
                s.live.ruler_drag = None;
            }
            if !mouse && s.live.pen_pan {
                s.live.pen_pan = false;
                s.live.pan = None;
            }
        }
        // A pan barrel button drags the view instead of drawing (R-CANVAS-002, R-PEN-010).
        PenEventKind::Down
            if !mouse && s.barrel(s.live.button) == Some(vibenote_model::settings::BarrelAction::Pan) =>
        {
            s.live.pen_pan = true;
            s.live.pan = Some((e.x, e.y));
        }
        PenEventKind::Motion if !mouse && s.live.pen_pan => {
            if let Some((px, py)) = s.live.pan.replace((e.x, e.y)) {
                pan_view(s, e.x - px, e.y - py);
            }
        }
        PenEventKind::Up if !mouse && s.live.pen_pan => {
            s.live.pen_pan = false;
            s.live.pan = None;
        }
        // The text tool presses into boxes like into a text field (R-TEXT-005); selection
        // handles (corners, width bar) still resize a selected box.
        PenEventKind::Down
            if e.tool == PenTool::Pen
                && (mouse || s.barrel(s.live.button).is_none())
                && s.settings.tool == vibenote_model::settings::ToolKind::Text
                && !matches!(
                    s.handle_at(s.view.to_canvas(e.x, e.y)),
                    Some(crate::Handle::Scale(_) | crate::Handle::Width | crate::Handle::Rotate)
                )
                && text_press(s, e, mouse, out) => {}
        PenEventKind::Motion if s.live.text_drag.is_some_and(|(_, m)| m == mouse) => {
            let at = s.view.to_canvas(e.x, e.y);
            super::text::drag_to(s, at);
            s.live.hover = Some(at);
        }
        PenEventKind::Up if s.live.text_drag.is_some_and(|(_, m)| m == mouse) => {
            s.live.text_drag = None;
        }
        // The ruler: its body moves it, its knob rotates it (R-RULER-001).
        PenEventKind::Down
            if e.tool == PenTool::Pen
                && (mouse || s.barrel(s.live.button).is_none())
                && let Some(part) = s.ruler.part_at(s.view.to_canvas(e.x, e.y), s.zoom()) =>
        {
            finish(s, out);
            s.live.ruler_drag = Some(super::shape::RulerDrag {
                part,
                start: s.view.to_canvas(e.x, e.y),
                ruler: s.ruler,
                mouse,
            });
        }
        PenEventKind::Motion if s.live.ruler_drag.is_some_and(|d| d.mouse == mouse) => {
            let at = s.view.to_canvas(e.x, e.y);
            super::shape::ruler_drag_to(s, at);
            s.live.hover = Some(at);
        }
        PenEventKind::Up if s.live.ruler_drag.is_some_and(|d| d.mouse == mouse) => {
            s.live.ruler_drag = None;
        }
        PenEventKind::Down => {
            if !press_selection(s, e, mouse) {
                begin(s, e, if mouse { None } else { s.live.button }, mouse, out);
            }
        }
        PenEventKind::Motion if dragging => {
            let at = s.view.to_canvas(e.x, e.y);
            let zoom = s.zoom();
            if let Some(d) = s.live.drag.as_mut() {
                d.update(at, zoom);
            }
            s.live.hover = Some(at);
        }
        PenEventKind::Up if dragging => {
            finish_drag(s, out);
            s.live.hover = Some(s.view.to_canvas(e.x, e.y));
        }
        PenEventKind::Motion => {
            let t0 = s.live.stroke.as_ref().filter(|l| l.mouse == mouse).map(|l| l.t0_us);
            match t0 {
                Some(t0) => {
                    let smp = sample(s, e, t0);
                    if let Some(ls) = s.live.stroke.as_mut()
                        && ls.samples.len() < vibenote_model::limits::MAX_SAMPLES_PER_STROKE
                    {
                        ls.samples.push(smp);
                    }
                }
                None => s.live.hover = Some(s.view.to_canvas(e.x, e.y)),
            }
        }
        PenEventKind::Up => {
            if mine(s) {
                finish(s, out);
                s.live.hover = Some(s.view.to_canvas(e.x, e.y));
            }
        }
        // Barrel buttons (R-PEN-010): 1 = temporary lasso, 2 = temporary eraser, while held.
        PenEventKind::ButtonDown if matches!(e.button, Some(BTN_STYLUS | BTN_STYLUS2)) => s.live.button = e.button,
        PenEventKind::ButtonUp if e.button == s.live.button => s.live.button = None,
        PenEventKind::ButtonDown | PenEventKind::ButtonUp => {}
    }
}

/// Two presses this close in time (µs) and space (screen px) are a double click.
const DOUBLE_CLICK_US: u64 = 400_000;
const DOUBLE_CLICK_PX: f64 = 8.0;

/// Linux codes of the mouse buttons.
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;
/// Pressure of mouse strokes (R-PEN-012).
const MOUSE_PRESSURE: f32 = 0.5;
/// Largest surface coordinate accepted from the mouse (hostile input).
const POINTER_MAX: f64 = 1.0e7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PointerKind {
    Down,
    Motion,
    Up,
    /// The pointer left the canvas (onto the chrome or out of the window): no footprint.
    Leave,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PointerEvent {
    t_us: u64,
    kind: PointerKind,
    /// Surface-local logical pixels.
    x: f64,
    y: f64,
    /// Linux button code for down/up (default BTN_LEFT = 272; BTN_MIDDLE = 274 pans).
    button: Option<u32>,
    /// Shift held (Shift+click extends a text selection).
    #[serde(default)]
    shift: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PointerBatch {
    events: Vec<PointerEvent>,
}

/// One mouse event: the left button draws like a pen tip at pressure 0.5 with the selected
/// tool, the middle button drags the view, other buttons are ignored.
fn pointer_step(s: &mut Session, e: &PointerEvent, out: &mut BatchResult) {
    s.live.shift = e.shift;
    let pen = |kind| PenEvent {
        t_us: e.t_us,
        kind,
        tool: PenTool::Pen,
        x: e.x,
        y: e.y,
        p: MOUSE_PRESSURE,
        tx: 0.0,
        ty: 0.0,
        d: 0.0,
        button: None,
    };
    match (e.kind, e.button.unwrap_or(BTN_LEFT)) {
        // While the pen is near, the mouse does not draw (palm on a touchpad).
        (PointerKind::Down, BTN_LEFT) if s.live.tool.is_some() => {}
        (PointerKind::Down, BTN_LEFT) => step(s, &pen(PenEventKind::Down), true, out),
        (PointerKind::Up, BTN_LEFT) => step(s, &pen(PenEventKind::Up), true, out),
        (PointerKind::Down, BTN_MIDDLE) => s.live.pan = Some((e.x, e.y)),
        (PointerKind::Up, BTN_MIDDLE) => s.live.pan = None,
        (PointerKind::Motion, _) => match s.live.pan {
            Some((px, py)) => {
                pan_view(s, e.x - px, e.y - py);
                s.live.pan = Some((e.x, e.y));
            }
            None => step(s, &pen(PenEventKind::Motion), true, out),
        },
        (PointerKind::Leave, _) => {
            if s.live.tool.is_none() {
                s.live.hover = None;
            }
        }
        (PointerKind::Down | PointerKind::Up, _) => {}
    }
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "input.pointer",
            doc: "Feed mouse events (surface coords): the left button draws with the selected tool at pressure 0.5 (or drags the selection by its box and handles), the middle button pans; motion without a button is hover, `leave` ends it.",
            reqs: &["R-PEN-012"],
            implemented: true,
        },
        |s: &mut Session, p: PointerBatch| {
            let ok = |v: f64| v.is_finite() && v.abs() < POINTER_MAX;
            if !p.events.iter().all(|e| ok(e.x) && ok(e.y)) {
                return Err(EngineError::InvalidParams {
                    cmd: "input.pointer".into(),
                    msg: "pointer position must be finite and within ±1e7".into(),
                });
            }
            let mut out = BatchResult::default();
            for e in &p.events {
                pointer_step(s, e, &mut out);
            }
            Ok(out)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "input.pen",
            doc: "Feed tablet frames (surface coords) to the pen state machine; strokes commit on pen-up.",
            reqs: &["R-PEN-001", "R-PEN-002", "R-PEN-004", "R-PEN-006", "R-PEN-011"],
            implemented: true,
        },
        |s: &mut Session, p: PenBatch| {
            // Validate the whole batch first: a rejected batch changes nothing.
            for e in &p.events {
                e.validate().map_err(|err| EngineError::InvalidParams {
                    cmd: "input.pen".into(),
                    msg: err.to_string(),
                })?;
            }
            let mut out = BatchResult::default();
            for e in &p.events {
                step(s, e, false, &mut out);
            }
            Ok(out)
        },
    )))?;
    Ok(())
}
