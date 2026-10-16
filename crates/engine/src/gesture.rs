//! Direct manipulation on the canvas: dragging the selection by its box and handles (move,
//! scale from a corner, rotate around the centre; R-PEN-009), and what the pointer would do at
//! its position ([`CursorHint`], R-PEN-011), which the UI shows as the system cursor or as a
//! footprint drawn in the overlay.
//!
//! A drag is previewed with a display transform on the selected strokes (like prettify, ADR
//! 0008) and committed as one `selection.transform` step when the pen or button is released.

use crate::{NoteId, Session};
use serde::Serialize;
use std::ops::Not;
use vibenote_geom::{Point, Rect};

/// Gap between the selection and its box, on screen (px).
pub(crate) const SELECTION_MARGIN_PX: f64 = 6.0;
/// Side of a corner handle, on screen (px).
pub(crate) const HANDLE_PX: f64 = 8.0;
/// How far a handle reacts around its centre, on screen (px).
pub(crate) const HANDLE_HIT_PX: f64 = 10.0;
/// Distance of the rotate knob above the box, on screen (px).
pub(crate) const ROTATE_STEM_PX: f64 = 24.0;
/// Radius of the rotate knob, on screen (px).
pub(crate) const ROTATE_KNOB_PX: f64 = 5.0;
/// Narrowest wrap width a text box can be dragged to (screen px).
const MIN_TEXT_WIDTH_PX: f64 = 24.0;
/// Movement (screen px) below which a press on the selection is a tap, not a drag.
const TAP_PX: f64 = 1.0;
/// Scale factors a corner drag may reach in one gesture.
const DRAG_SCALE: (f64, f64) = (0.05, 20.0);

/// Corners of the selection box, in drawing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Corner {
    const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomRight,
        Corner::BottomLeft,
    ];

    fn of(self, r: &Rect) -> Point {
        match self {
            Corner::TopLeft => r.min,
            Corner::TopRight => Point::new(r.max.x, r.min.y),
            Corner::BottomRight => r.max,
            Corner::BottomLeft => Point::new(r.min.x, r.max.y),
        }
    }

    fn opposite(self) -> Corner {
        match self {
            Corner::TopLeft => Corner::BottomRight,
            Corner::TopRight => Corner::BottomLeft,
            Corner::BottomRight => Corner::TopLeft,
            Corner::BottomLeft => Corner::TopRight,
        }
    }
}

/// What part of the selection a press grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "corner")]
pub enum Handle {
    /// Inside the box: move.
    Move,
    /// A corner: scale uniformly, the opposite corner stays put.
    Scale(Corner),
    /// The knob above the box: rotate around the centre.
    Rotate,
    /// The middle of the right edge of a single text box: its wrap width.
    Width,
}

/// `x' = pivot + translate + R(rotate)·S(scale)·(x − pivot)`, the parameters of
/// `selection.transform`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Xform {
    pub translate: (f64, f64),
    pub scale: f64,
    pub rotate_deg: f64,
    pub pivot: (f64, f64),
}

impl Default for Xform {
    fn default() -> Self {
        Xform {
            translate: (0.0, 0.0),
            scale: 1.0,
            rotate_deg: 0.0,
            pivot: (0.0, 0.0),
        }
    }
}

impl Xform {
    /// As an affine `[a, b, c, d, e, f]`: (x, y) ↦ (a·x + c·y + e, b·x + d·y + f).
    pub fn affine(&self) -> [f64; 6] {
        let (sin, cos) = self.rotate_deg.to_radians().sin_cos();
        let k = self.scale;
        let (a, b, c, d) = (k * cos, k * sin, -k * sin, k * cos);
        let (px, py) = self.pivot;
        let (tx, ty) = self.translate;
        [a, b, c, d, px + tx - (a * px + c * py), py + ty - (b * px + d * py)]
    }

    pub fn is_identity(&self) -> bool {
        self.translate == (0.0, 0.0) && self.scale == 1.0 && self.rotate_deg == 0.0
    }
}

/// `p ∘ t`: first `t`, then `p`.
pub(crate) fn compose(p: &[f64; 6], t: &[f64; 6]) -> [f64; 6] {
    [
        p[0] * t[0] + p[2] * t[1],
        p[1] * t[0] + p[3] * t[1],
        p[0] * t[2] + p[2] * t[3],
        p[1] * t[2] + p[3] * t[3],
        p[0] * t[4] + p[2] * t[5] + p[4],
        p[1] * t[4] + p[3] * t[5] + p[5],
    ]
}

/// A drag of the selection in progress.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Drag {
    pub note: NoteId,
    pub handle: Handle,
    /// Driven by the mouse (pen events never move it, and vice versa).
    pub mouse: bool,
    /// Where the press was (canvas).
    pub start: (f64, f64),
    /// The selection's box when the drag began (canvas, without margin).
    pub rect: Rect,
    /// The transform so far.
    pub xform: Xform,
    /// The pointer moved further than a tap.
    pub moved: bool,
    /// A text box's wrap width when the drag began, and the new one ([`Handle::Width`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_width: Option<(f64, f64)>,
}

impl Drag {
    /// Updates the transform for the pointer at `at` (canvas units); `zoom` tells what a
    /// screen pixel is.
    pub(crate) fn update(&mut self, at: (f64, f64), zoom: f64) {
        let (dx, dy) = (at.0 - self.start.0, at.1 - self.start.1);
        if dx.hypot(dy) * zoom > TAP_PX {
            self.moved = true;
        }
        if !self.moved {
            return;
        }
        let c = Point::new(
            f64::midpoint(self.rect.min.x, self.rect.max.x),
            f64::midpoint(self.rect.min.y, self.rect.max.y),
        );
        if let (Handle::Width, Some((w0, _))) = (self.handle, self.text_width) {
            self.text_width = Some((w0, (w0 + dx).max(px(zoom, MIN_TEXT_WIDTH_PX))));
            return;
        }
        self.xform = match self.handle {
            Handle::Width => Xform::default(),
            Handle::Move => Xform {
                translate: (dx, dy),
                pivot: (c.x, c.y),
                ..Xform::default()
            },
            Handle::Scale(corner) => {
                let anchor = corner.opposite().of(&self.rect);
                let from = corner.of(&self.rect);
                let (vx, vy) = (from.x - anchor.x, from.y - anchor.y);
                let len2 = vx * vx + vy * vy;
                // The dragged corner follows the pointer along the diagonal; the opposite
                // corner stays put.
                let k = if len2 > 0.0 && len2.is_finite() {
                    1.0 + (dx * vx + dy * vy) / len2
                } else {
                    1.0
                };
                let k = if k.is_finite() {
                    k.clamp(DRAG_SCALE.0, DRAG_SCALE.1)
                } else {
                    1.0
                };
                Xform {
                    scale: k,
                    pivot: (anchor.x, anchor.y),
                    ..Xform::default()
                }
            }
            Handle::Rotate => {
                let a0 = (self.start.1 - c.y).atan2(self.start.0 - c.x);
                let a1 = (at.1 - c.y).atan2(at.0 - c.x);
                let deg = (a1 - a0).to_degrees();
                Xform {
                    rotate_deg: if deg.is_finite() { deg } else { 0.0 },
                    pivot: (c.x, c.y),
                    ..Xform::default()
                }
            }
        };
    }
}

/// What the pointer would do where it is; the UI picks the system cursor from it, and draws
/// [`CursorHint::Ink`] / [`CursorHint::Erase`] as a footprint (R-PEN-011).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorHint {
    Default,
    /// Pen or highlighter: a dot as wide as the ink.
    Ink,
    /// An eraser: a ring as large as the eraser.
    Erase,
    Lasso,
    Shape,
    Text,
    /// Over the selection: it can be moved.
    Move,
    /// Over a corner handle: scale (the corner says which diagonal).
    Scale(Corner),
    /// Over the rotate knob.
    Rotate,
    /// Over a text box's width handle.
    ResizeWidth,
    /// Something is being dragged (the selection, or the view).
    Grabbing,
}

/// A screen distance in canvas units at `zoom` (guards a broken zoom).
pub(crate) fn px(zoom: f64, v: f64) -> f64 {
    if zoom.is_finite() && zoom > 0.0 { v / zoom } else { v }
}

impl Session {
    pub(crate) fn zoom(&self) -> f64 {
        if self.view.zoom.is_finite() && self.view.zoom > 0.0 {
            self.view.zoom
        } else {
            1.0
        }
    }

    /// Bounds of the selection in the current note as displayed (prettify transforms
    /// included, without the drag in progress), or `None` without a selection there.
    pub fn selection_rect(&self) -> Option<Rect> {
        let (Some(note), Some(cur)) = (self.selection.note, self.current_note) else {
            return None;
        };
        if note != cur || self.selection.ids.is_empty() {
            return None;
        }
        let canvas = self.ws.canvas(note).ok()?;
        // FNV-1a over the ids: cheap per frame; the box itself is recomputed only when the
        // selection or the note changed.
        let h = self
            .selection
            .ids
            .iter()
            .chain(std::iter::once(&self.prettify_rev))
            .fold(0xcbf2_9ce4_8422_2325u64, |h, id| {
                (h ^ id).wrapping_mul(0x0000_0100_0000_01b3)
            });
        let key = self.logs.get(&note).map(|l| (l.epoch(), l.rev(), h));
        if let Some(r) = key.and_then(|k| self.overlay_cache.get(k)) {
            return (!r.is_empty()).then_some(r);
        }
        let display = self.prettify_transforms(note);
        let mut r = Rect::EMPTY;
        for id in &self.selection.ids {
            let Some(o) = canvas.objects.get(&vibenote_model::ObjectId(*id)) else {
                continue;
            };
            let b = match &o.kind {
                vibenote_model::ObjectKind::Stroke(st) => st.bounds(),
                _ => self.object_box(o).unwrap_or(Rect::EMPTY),
            };
            let b = match display.get(&vibenote_model::ObjectId(*id)) {
                Some(t) if !b.is_empty() => crate::hwr::map_rect(t, &b),
                _ => b,
            };
            if !b.is_empty() {
                r.include(b.min);
                r.include(b.max);
            }
        }
        if let Some(k) = key {
            self.overlay_cache.put(k, r);
        }
        (!r.is_empty()).then_some(r)
    }

    /// The selection's box on screen (margin included), as drawn: its corners in order
    /// (transformed by a drag in progress) and its axis-aligned original.
    fn selection_frame(&self) -> Option<([Point; 4], Rect)> {
        let r = self.selection_rect()?;
        let r = r.inflate(px(self.zoom(), SELECTION_MARGIN_PX));
        let t = self.live.drag.as_ref().map(|d| d.xform.affine());
        // A text box being widened grows to the right.
        let grow = match self.live.drag.as_ref().and_then(|d| d.text_width) {
            Some((w0, w)) => w - w0,
            None => 0.0,
        };
        let corners = Corner::ALL.map(|c| {
            let mut p = c.of(&r);
            if matches!(c, Corner::TopRight | Corner::BottomRight) {
                p.x += grow;
            }
            match &t {
                Some(t) => {
                    let (x, y) = crate::hwr::map_point(t, (p.x, p.y));
                    Point::new(x, y)
                }
                None => p,
            }
        });
        Some((corners, r))
    }

    /// For the selection's toolbar: whether it holds ink, and whether any of that ink is shown
    /// prettified.
    pub fn selection_ink(&self) -> (bool, bool) {
        let Some(note) = self.selection.note.filter(|n| Some(*n) == self.current_note) else {
            return (false, false);
        };
        let Ok(c) = self.ws.canvas(note) else {
            return (false, false);
        };
        let ink: Vec<vibenote_model::ObjectId> = self
            .selection
            .ids
            .iter()
            .map(|i| vibenote_model::ObjectId(*i))
            .filter(|id| {
                matches!(
                    c.objects.get(id).map(|o| &o.kind),
                    Some(vibenote_model::ObjectKind::Stroke(_))
                )
            })
            .collect();
        let tidied = self
            .ws
            .prettified
            .get(&note)
            .is_some_and(|m| m.values().any(|b| b.strokes.iter().any(|id| ink.contains(id))));
        (!ink.is_empty(), tidied)
    }

    /// The selected object if it is exactly one text box: its id and wrap width.
    pub(crate) fn single_text(&self) -> Option<(u64, f64)> {
        let [id] = self.selection.ids.as_slice() else {
            return None;
        };
        let note = self.selection.note?;
        match self
            .ws
            .canvas(note)
            .ok()?
            .objects
            .get(&vibenote_model::ObjectId(*id))
            .map(|o| &o.kind)
        {
            Some(vibenote_model::ObjectKind::Text { width, .. }) => Some((*id, *width)),
            _ => None,
        }
    }

    /// The width handle of a single selected text box: the middle of the box's right edge.
    fn width_handle(c: &[Point; 4]) -> Point {
        Point::new(f64::midpoint(c[1].x, c[2].x), f64::midpoint(c[1].y, c[2].y))
    }

    /// The rotate knob's centre for a box with these corners: above the middle of the top
    /// edge, perpendicular to it.
    fn knob(&self, c: &[Point; 4]) -> Point {
        let [tl, tr, br, _] = *c;
        let top = Point::new(f64::midpoint(tl.x, tr.x), f64::midpoint(tl.y, tr.y));
        // Unit vector from the bottom edge towards the top edge.
        let (ux, uy) = (tr.x - br.x, tr.y - br.y);
        let len = ux.hypot(uy);
        let (ux, uy) = if len > 0.0 && len.is_finite() {
            (ux / len, uy / len)
        } else {
            (0.0, -1.0)
        };
        let stem = px(self.zoom(), ROTATE_STEM_PX);
        Point::new(top.x + ux * stem, top.y + uy * stem)
    }

    /// The handle of the selection under `at` (canvas units), if any.
    pub fn handle_at(&self, at: (f64, f64)) -> Option<Handle> {
        let (corners, r) = self.selection_frame()?;
        let hit = px(self.zoom(), HANDLE_HIT_PX);
        let p = Point::new(at.0, at.1);
        if self.rotatable() && self.knob(&corners).distance(p) <= hit {
            return Some(Handle::Rotate);
        }
        for (c, q) in Corner::ALL.iter().zip(corners) {
            if q.distance(p) <= hit {
                return Some(Handle::Scale(*c));
            }
        }
        if self.single_text().is_some() && Self::width_handle(&corners).distance(p) <= hit {
            return Some(Handle::Width);
        }
        r.contains(p).then_some(Handle::Move)
    }

    /// Starts dragging the selection if `at` (canvas) is on it.
    /// Starts moving the selection from `at` wherever the press is (a text box's frame band).
    pub(crate) fn begin_move(&mut self, at: (f64, f64), mouse: bool) -> bool {
        let (Some(note), Some(rect)) = (self.selection.note, self.selection_rect()) else {
            return false;
        };
        self.live.drag = Some(Drag {
            note,
            handle: Handle::Move,
            mouse,
            start: at,
            rect,
            xform: Xform::default(),
            moved: false,
            text_width: None,
        });
        true
    }

    pub(crate) fn begin_drag(&mut self, at: (f64, f64), mouse: bool) -> bool {
        let Some(handle) = self.handle_at(at) else { return false };
        let (Some(note), Some(rect)) = (self.selection.note, self.selection_rect()) else {
            return false;
        };
        self.live.drag = Some(Drag {
            note,
            handle,
            mouse,
            start: at,
            rect,
            xform: Xform::default(),
            moved: false,
            text_width: self
                .single_text()
                .filter(|_| handle == Handle::Width)
                .map(|(_, w)| (w, w)),
        });
        true
    }

    /// Display transforms of `note`: prettify (ADR 0008), and the selection drag in progress
    /// on top (`prettify ∘ drag`, which is what committing the drag stores).
    pub fn display_transforms(&self, note: NoteId) -> std::collections::HashMap<vibenote_model::ObjectId, [f64; 6]> {
        let mut out = self.prettify_transforms(note);
        if let Some(d) = self.live.drag.as_ref().filter(|d| d.note == note && d.moved) {
            let t = d.xform.affine();
            for id in &self.selection.ids {
                let id = vibenote_model::ObjectId(*id);
                let m = out.get(&id).map_or(t, |p| compose(p, &t));
                out.insert(id, m);
            }
        }
        out
    }

    /// `o` where the selection drag in progress would put it (`None`: not dragged).
    pub(crate) fn drag_preview(
        &self,
        note: NoteId,
        o: &vibenote_model::CanvasObject,
    ) -> Option<vibenote_model::CanvasObject> {
        let d = self.live.drag.as_ref().filter(|d| d.note == note && d.moved)?;
        if !self.selection.ids.contains(&o.id.0) {
            return None;
        }
        if let Some((_, w)) = d.text_width {
            let mut o = o.clone();
            if let vibenote_model::ObjectKind::Text { width, .. } = &mut o.kind {
                *width = w;
            }
            return Some(o);
        }
        let p = crate::commands::selection::xform_params(&d.xform);
        crate::commands::selection::transformed(std::slice::from_ref(o), &p)
            .ok()?
            .into_iter()
            .next()
    }

    /// True if every selected object can be rotated (ink and shapes).
    fn rotatable(&self) -> bool {
        let Some(note) = self.selection.note else { return false };
        let Ok(canvas) = self.ws.canvas(note) else { return false };
        self.selection.ids.iter().all(|id| {
            canvas.objects.get(&vibenote_model::ObjectId(*id)).is_none_or(|o| {
                matches!(
                    o.kind,
                    vibenote_model::ObjectKind::Stroke(_) | vibenote_model::ObjectKind::Shape { .. }
                )
            })
        })
    }

    /// What the pointer would do at the last hover position (or during the current gesture).
    pub fn cursor_hint(&self) -> CursorHint {
        use vibenote_ink::input::PenTool;
        use vibenote_model::settings::{BarrelAction, ToolKind};
        if self.live.drag.is_some() || self.live.pan.is_some() {
            return CursorHint::Grabbing;
        }
        if let Some(ls) = &self.live.stroke {
            if self.erasing(ls).is_some() {
                return CursorHint::Erase;
            }
            if self.lassoing(ls) {
                return CursorHint::Lasso;
            }
        }
        if self.live.tool == Some(PenTool::Eraser) {
            return CursorHint::Erase;
        }
        match self.barrel(self.live.button) {
            Some(BarrelAction::Eraser) => return CursorHint::Erase,
            Some(BarrelAction::Lasso) => return CursorHint::Lasso,
            Some(BarrelAction::Pan) => return CursorHint::Grabbing,
            _ => {}
        }
        // The ruler: its body moves, its knob rotates (R-RULER-001).
        if let Some(d) = self.live.ruler_drag {
            return match d.part {
                crate::commands::shape::RulerPart::Body => CursorHint::Move,
                crate::commands::shape::RulerPart::Knob => CursorHint::Rotate,
            };
        }
        if self.live.stroke.is_none()
            && let Some(at) = self.live.hover
            && let Some(part) = self.ruler.part_at(at, self.zoom())
        {
            return match part {
                crate::commands::shape::RulerPart::Body => CursorHint::Move,
                crate::commands::shape::RulerPart::Knob => CursorHint::Rotate,
            };
        }
        if self.live.stroke.is_none()
            && let Some(at) = self.live.hover
            && let Some(h) = self.handle_at(at)
        {
            return match h {
                Handle::Move => CursorHint::Move,
                Handle::Scale(c) => CursorHint::Scale(c),
                Handle::Rotate => CursorHint::Rotate,
                Handle::Width => CursorHint::ResizeWidth,
            };
        }
        match self.settings.tool {
            ToolKind::Pen | ToolKind::Highlighter | ToolKind::Cover => CursorHint::Ink,
            ToolKind::EraserStroke | ToolKind::EraserArea => CursorHint::Erase,
            ToolKind::Lasso => CursorHint::Lasso,
            ToolKind::Shape => CursorHint::Shape,
            ToolKind::Text => CursorHint::Text,
        }
    }

    /// Overlay strokes for the selection box with its handles, and the tool's footprint at the
    /// hover position (R-PEN-011).
    pub(crate) fn overlay_controls(&self, out: &mut Vec<vibenote_model::Stroke>) {
        let zoom = self.zoom();
        if let Some((c, _)) = self.selection_frame() {
            let [tl, tr, br, bl] = c;
            out.push(overlay_line(
                &[tl, tr, br, bl, tl],
                crate::OVERLAY_COLOR,
                crate::OVERLAY_PX / zoom,
            ));
            // Corner handles: small squares aligned with the box.
            let h = px(zoom, HANDLE_PX) / 2.0;
            let (ux, uy) = unit(tl, tr);
            let (vx, vy) = (-uy, ux);
            for q in c {
                let p = |a: f64, b: f64| Point::new(q.x + (a * ux + b * vx) * h, q.y + (a * uy + b * vy) * h);
                let sq = [p(-1.0, -1.0), p(1.0, -1.0), p(1.0, 1.0), p(-1.0, 1.0), p(-1.0, -1.0)];
                out.push(overlay_line(&sq, crate::OVERLAY_COLOR, 2.0 * crate::OVERLAY_PX / zoom));
            }
            // A single text box: a bar on its right edge sets the wrap width.
            if self.single_text().is_some() {
                let m = Self::width_handle(&c);
                let (vx, vy) = (vx * 1.6 * h, vy * 1.6 * h);
                out.push(overlay_line(
                    &[Point::new(m.x - vx, m.y - vy), Point::new(m.x + vx, m.y + vy)],
                    crate::OVERLAY_COLOR,
                    px(zoom, 4.0),
                ));
            }
            // The rotate knob on its stem (only ink and shapes rotate).
            let k = self.knob(&c);
            if !self.rotatable() {
                return self.overlay_footprint(out);
            }
            let top = Point::new(f64::midpoint(tl.x, tr.x), f64::midpoint(tl.y, tr.y));
            out.push(overlay_line(&[top, k], crate::OVERLAY_COLOR, crate::OVERLAY_PX / zoom));
            out.push(overlay_line(
                &circle(k, px(zoom, ROTATE_KNOB_PX)),
                crate::OVERLAY_COLOR,
                2.0 * crate::OVERLAY_PX / zoom,
            ));
        }
        self.overlay_footprint(out);
    }

    /// The shape the shape tool would make: the drag in progress, or a polygon's corners so
    /// far with a rubber band to the pen (R-SHAPE-001).
    fn overlay_shape(&self, out: &mut Vec<vibenote_model::Stroke>) {
        use vibenote_model::settings::ToolKind;
        if self.settings.tool != ToolKind::Shape {
            return;
        }
        let ink = crate::pen_stroke(Vec::new());
        let live = self
            .live
            .stroke
            .as_ref()
            .filter(|l| self.lassoing(l).not() && self.erasing(l).is_none());
        let ends = live.and_then(|l| Some((l.samples.first()?, l.samples.last()?)));
        if let Some((a, b)) = ends
            && let Some((points, closed)) =
                crate::commands::input::drag_shape(self.settings.shape, (a.x, a.y), (b.x, b.y))
        {
            for line in vibenote_model::shape_outline(self.settings.shape, &points, closed, ink.width) {
                out.push(overlay_line(&line, ink.color, f64::from(ink.width)));
            }
            return;
        }
        if self.live.polygon.is_empty() {
            return;
        }
        let mut pts: Vec<Point> = self.live.polygon.iter().map(|&(x, y)| Point::new(x, y)).collect();
        let to = ends.map(|(_, b)| (b.x, b.y)).or(self.live.hover);
        if let Some((x, y)) = to {
            pts.push(Point::new(x, y));
        }
        out.push(overlay_line(&pts, ink.color, f64::from(ink.width)));
        // The first corner: tap it to close the polygon.
        if let Some(f) = pts.first() {
            out.push(overlay_line(
                &circle(*f, px(self.zoom(), 6.0)),
                crate::OVERLAY_COLOR,
                px(self.zoom(), 1.5),
            ));
        }
    }

    /// The tool's footprint at the hover position (R-PEN-011).
    fn overlay_footprint(&self, out: &mut Vec<vibenote_model::Stroke>) {
        self.overlay_shape(out);
        let zoom = self.zoom();
        if self.live.stroke.is_some() || self.live.drag.is_some() {
            return;
        }
        let Some((x, y)) = self.live.hover else { return };
        let at = Point::new(x, y);
        match self.cursor_hint() {
            CursorHint::Erase => {
                out.push(overlay_line(
                    &circle(at, px(zoom, crate::ERASER_RADIUS_PX)),
                    FOOTPRINT_RING,
                    px(zoom, 1.5),
                ));
            }
            CursorHint::Ink => {
                use vibenote_model::settings::ToolKind;
                let (color, width) = match self.settings.tool {
                    ToolKind::Highlighter => (crate::HIGHLIGHTER_COLOR, 12.0),
                    _ => {
                        let s = crate::pen_stroke(Vec::new());
                        (s.color, f64::from(s.width))
                    }
                };
                // A dot as wide as the ink at medium pressure, never smaller than 3 px.
                let d = (width * 0.7).max(px(zoom, 3.0));
                let mut dot = overlay_line(&[at, at], dim(color), d);
                dot.tool = vibenote_model::Tool::Pen;
                out.push(dot);
            }
            _ => {}
        }
    }
}

/// Grey ring of the eraser footprint.
const FOOTPRINT_RING: vibenote_model::Color = vibenote_model::Color(0x6a6a6ad0);

/// `c` at about two thirds of its opacity (a footprint is not ink).
fn dim(c: vibenote_model::Color) -> vibenote_model::Color {
    let a = c.0 & 0xff;
    vibenote_model::Color((c.0 & !0xff) | (a.saturating_mul(2) / 3))
}

fn unit(a: Point, b: Point) -> (f64, f64) {
    let (x, y) = (b.x - a.x, b.y - a.y);
    let l = x.hypot(y);
    if l > 0.0 && l.is_finite() {
        (x / l, y / l)
    } else {
        (1.0, 0.0)
    }
}

/// A closed polyline approximating a circle.
fn circle(c: Point, r: f64) -> Vec<Point> {
    (0..=24)
        .map(|i| {
            let a = f64::from(i) / 24.0 * std::f64::consts::TAU;
            Point::new(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation)] // a few pixels in canvas units
pub(crate) fn overlay_line(pts: &[Point], color: vibenote_model::Color, width: f64) -> vibenote_model::Stroke {
    vibenote_model::Stroke {
        tool: vibenote_model::Tool::Pen,
        color,
        width: width as f32,
        samples: pts
            .iter()
            .map(|p| vibenote_model::Sample {
                x: p.x,
                y: p.y,
                pressure: 1.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
                t_us: 0,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_matches_the_transform_definition() {
        let x = Xform {
            translate: (3.0, -2.0),
            scale: 1.5,
            rotate_deg: 30.0,
            pivot: (10.0, 20.0),
        };
        let t = x.affine();
        let p = (14.0, 25.0);
        let (sin, cos) = 30f64.to_radians().sin_cos();
        let (dx, dy) = ((p.0 - 10.0) * 1.5, (p.1 - 20.0) * 1.5);
        let want = (10.0 + 3.0 + dx * cos - dy * sin, 20.0 - 2.0 + dx * sin + dy * cos);
        let got = crate::hwr::map_point(&t, p);
        assert!((got.0 - want.0).abs() < 1e-9 && (got.1 - want.1).abs() < 1e-9);
    }

    #[test]
    fn compose_applies_the_right_side_first() {
        let t = Xform {
            translate: (5.0, 0.0),
            ..Xform::default()
        }
        .affine();
        let p = Xform {
            scale: 2.0,
            ..Xform::default()
        }
        .affine();
        let got = crate::hwr::map_point(&compose(&p, &t), (1.0, 1.0));
        assert_eq!(got, (12.0, 2.0));
    }
}
