//! Text boxes (R-TEXT-001/002/003): create, edit and format rich text. Positions are character
//! (Unicode scalar) offsets; every change is one undo step in the note.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::Point;
use vibenote_model::{CanvasObject, Color, ObjectId, ObjectKind, Op, TextMark, TextStyle};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateP {
    at: [f64; 2],
    /// Wrap width (canvas units).
    width: f64,
    #[serde(default)]
    content: String,
    /// Font size (canvas units per em, default 16).
    size: Option<f32>,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditP {
    id: u64,
    /// Text to insert at `at` (character offset; default: the end).
    insert: Option<String>,
    at: Option<u32>,
    /// Character range to delete (`[start, end)`), applied before the insertion.
    delete: Option<[u32; 2]>,
    note: Option<u64>,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FormatP {
    Bold,
    Italic,
    Underline,
    Strike,
    Heading1,
    Heading2,
    Heading3,
    BulletList,
    NumberedList,
    Checkbox,
    Color,
    Size,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FormatArgs {
    id: u64,
    format: FormatP,
    /// Character range `[start, end)`.
    range: [u32; 2],
    /// `color`: 0xRRGGBBAA; `size`: canvas units per em; `checkbox`: 1 = checked.
    value: Option<f64>,
    /// Remove this format from the range instead of adding it.
    #[serde(default)]
    clear: bool,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetP {
    id: u64,
    note: Option<u64>,
}

#[derive(Serialize)]
struct Created {
    id: u64,
}

#[derive(Serialize)]
struct TextOut {
    content: String,
    marks: Vec<TextMark>,
    origin: Point,
    width: f64,
    size: f32,
    /// The box as laid out: [x0, y0, x1, y1] (R-TEXT-004).
    frame: Option<[f64; 4]>,
}

/// Where offset `x` goes when `[a, b)` is deleted.
fn after_delete(x: u32, a: u32, b: u32) -> u32 {
    if x <= a {
        x
    } else if x >= b {
        x.saturating_sub(b.saturating_sub(a))
    } else {
        a
    }
}

/// Byte index of character offset `c` in `s` (the end if `c` is past it).
fn byte_at(s: &str, c: u32) -> usize {
    s.char_indices()
        .nth(usize::try_from(c).unwrap_or(usize::MAX))
        .map_or(s.len(), |(i, _)| i)
}

/// Same-format key for merging (a colour or size value is part of the format).
fn same(a: &TextStyle, b: &TextStyle) -> bool {
    a == b
}

/// Sorts marks and merges overlapping or touching marks of the same format; drops empty ones.
fn normalise(marks: &mut Vec<TextMark>) {
    marks.retain(|m| m.start < m.end);
    marks.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    let mut out: Vec<TextMark> = Vec::with_capacity(marks.len());
    for m in marks.drain(..) {
        if let Some(prev) = out
            .iter_mut()
            .rev()
            .find(|p| same(&p.style, &m.style) && p.end >= m.start)
        {
            prev.end = prev.end.max(m.end);
        } else {
            out.push(m);
        }
    }
    *marks = out;
}

/// Removes `style` (of the same kind; any colour/size/checkbox value) from `[a, b)`.
fn clear(marks: &mut Vec<TextMark>, kind: std::mem::Discriminant<TextStyle>, a: u32, b: u32) {
    let mut out = Vec::with_capacity(marks.len());
    for m in marks.drain(..) {
        if std::mem::discriminant(&m.style) != kind || m.end <= a || m.start >= b {
            out.push(m);
            continue;
        }
        if m.start < a {
            out.push(TextMark { end: a, ..m.clone() });
        }
        if m.end > b {
            out.push(TextMark { start: b, ..m });
        }
    }
    *marks = out;
}

/// Deletes a character range and/or inserts text at an offset (default: the end); marks
/// follow the text.
fn splice(
    content: &mut String,
    marks: &mut Vec<TextMark>,
    delete: Option<[u32; 2]>,
    insert: Option<&str>,
    at: Option<u32>,
) -> Result<(), EngineError> {
    let len = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
    if let Some([a, b]) = delete {
        if a > b || b > len {
            return Err(EngineError::Model(format!("delete range {a}..{b} outside 0..{len}")));
        }
        let (ba, bb) = (byte_at(content, a), byte_at(content, b));
        content.replace_range(ba..bb, "");
        for m in marks.iter_mut() {
            m.start = after_delete(m.start, a, b);
            m.end = after_delete(m.end, a, b);
        }
    }
    if let Some(ins) = insert {
        let len = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
        let at = at.unwrap_or(len);
        if at > len {
            return Err(EngineError::Model(format!("offset {at} outside 0..{len}")));
        }
        let k = u32::try_from(ins.chars().count()).map_err(|_| EngineError::Model("insertion too long".into()))?;
        content.insert_str(byte_at(content, at), ins);
        for m in marks.iter_mut() {
            if m.start >= at {
                m.start = m.start.saturating_add(k);
                m.end = m.end.saturating_add(k);
            } else if at < m.end {
                m.end = m.end.saturating_add(k);
            }
        }
    }
    normalise(marks);
    Ok(())
}

fn text_object(
    s: &mut Session,
    note: Option<u64>,
    id: u64,
) -> Result<(vibenote_model::NoteId, CanvasObject), EngineError> {
    let note = super::ink::target_note(s, note)?;
    let o = super::pdf::object(s, note, id)?;
    if !matches!(o.kind, ObjectKind::Text { .. }) {
        return Err(EngineError::Model(format!("object {id} is not a text box")));
    }
    Ok((note, o))
}

/// The style a `text.format` request stands for.
fn style_of(format: FormatP, value_p: Option<f64>, clearing: bool) -> Result<TextStyle, EngineError> {
    let value = || value_p.ok_or_else(|| EngineError::Model("this format needs a value".into()));
    Ok(match format {
        FormatP::Bold => TextStyle::Bold,
        FormatP::Italic => TextStyle::Italic,
        FormatP::Underline => TextStyle::Underline,
        FormatP::Strike => TextStyle::Strike,
        FormatP::Heading1 => TextStyle::Heading1,
        FormatP::Heading2 => TextStyle::Heading2,
        FormatP::Heading3 => TextStyle::Heading3,
        FormatP::BulletList => TextStyle::BulletList,
        FormatP::NumberedList => TextStyle::NumberedList,
        FormatP::Checkbox => TextStyle::Checkbox {
            checked: value_p.is_some_and(|v| v != 0.0),
        },
        FormatP::Color if clearing => TextStyle::Color { color: Color(0) },
        FormatP::Color => {
            let v = value()?;
            if !(v.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&v) && v.fract() == 0.0) {
                return Err(EngineError::Model("colour must be 0xRRGGBBAA".into()));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked above
            TextStyle::Color { color: Color(v as u32) }
        }
        FormatP::Size if clearing => TextStyle::Size { size: 16.0 },
        FormatP::Size => {
            #[allow(clippy::cast_possible_truncation)] // the model checks the range
            TextStyle::Size { size: value()? as f32 }
        }
    })
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.create",
            doc: "Create a text box at a canvas point with a wrap width (one undo step).",
            reqs: &["R-TEXT-001", "R-TEXT-003", "R-CANVAS-003"],
            implemented: true,
        },
        |s: &mut Session, p: CreateP| {
            let note = super::ink::target_note(s, p.note)?;
            let z = s.next_z(note)?;
            let id = ObjectId(s.ws.alloc_id()?);
            let object = CanvasObject {
                id,
                z,
                kind: ObjectKind::Text {
                    origin: Point::new(p.at[0], p.at[1]),
                    width: p.width,
                    content: p.content,
                    marks: Vec::new(),
                    size: p.size.unwrap_or_else(vibenote_model::default_text_size),
                },
            };
            s.commit(Scope::Note(note), "text box", vec![Op::InsertObject { note, object }])?;
            Ok(Created { id: id.0 })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.edit",
            doc: "Delete a character range and/or insert text at a character offset; formatting follows the text (one undo step).",
            reqs: &["R-TEXT-001", "R-TEXT-002"],
            implemented: true,
        },
        |s: &mut Session, p: EditP| {
            let (note, mut o) = text_object(s, p.note, p.id)?;
            let ObjectKind::Text { content, marks, .. } = &mut o.kind else {
                return Err(EngineError::Model("not a text box".into()));
            };
            splice(content, marks, p.delete, p.insert.as_deref(), p.at)?;
            normalise(marks);
            s.commit(Scope::Note(note), "edit text", vec![Op::ReplaceObject { note, after: o }])?;
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.format",
            doc: "Add (or with `clear`, remove) a format over a character range: bold, italic, underline, strike, heading1–3, bullet_list, numbered_list, checkbox, color, size (one undo step).",
            reqs: &["R-TEXT-001"],
            implemented: true,
        },
        |s: &mut Session, p: FormatArgs| {
            let (note, mut o) = text_object(s, p.note, p.id)?;
            let ObjectKind::Text { content, marks, .. } = &mut o.kind else {
                return Err(EngineError::Model("not a text box".into()));
            };
            let len = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
            let [a, b] = p.range;
            if a >= b || b > len {
                return Err(EngineError::Model(format!("range {a}..{b} outside 0..{len} or empty")));
            }
            let style = style_of(p.format, p.value, p.clear)?;
            let kind = std::mem::discriminant(&style);
            // One value per range: a new colour/size/checkbox replaces the old one there.
            clear(marks, kind, a, b);
            if !p.clear {
                marks.push(TextMark { start: a, end: b, style });
            }
            normalise(marks);
            s.commit(Scope::Note(note), "format text", vec![Op::ReplaceObject { note, after: o }])?;
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.get",
            doc: "A text box's content, formatting, position, wrap width, size and laid-out frame.",
            reqs: &["R-TEXT-001", "R-TEXT-002"],
            implemented: true,
        },
        |s: &mut Session, p: GetP| {
            let (_, o) = text_object(s, p.note, p.id)?;
            let frame = s.object_box(&o).map(|b| [b.min.x, b.min.y, b.max.x, b.max.y]);
            let ObjectKind::Text {
                origin,
                width,
                content,
                marks,
                size,
            } = o.kind
            else {
                return Err(EngineError::Model("not a text box".into()));
            };
            Ok(TextOut {
                content,
                marks,
                origin,
                width,
                size,
                frame,
            })
        },
    )))?;
    register_editing(r)
}

/// A text box being typed into with the text tool (R-TEXT-001/002). Offsets are characters.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextEdit {
    pub note: vibenote_model::NoteId,
    /// The box; `None` until the first character creates it (an untouched box is never stored).
    pub id: Option<u64>,
    /// Where a new box goes, its wrap width and font size (canvas units).
    pub origin: Point,
    pub width: f64,
    pub size: f32,
    /// Caret and the other end of the selection (equal: nothing selected).
    pub caret: u32,
    pub anchor: u32,
    /// A tap in the box the UI places the caret for with the real layout (`text.caret`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<(f64, f64)>,
    /// With `place`: select the word there (a double click).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub word: bool,
    /// With `place`: select the line there (a triple click).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub line: bool,
    /// With `place`: where a drag started (it selects from there to `place`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place_from: Option<(f64, f64)>,
    /// With `place`: keep the selection's other end (Shift+click).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub extend: bool,
    /// Vim mode state (R-VIM-001); `None` without vim mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vim: Option<super::vim::VimState>,
}

impl TextEdit {
    pub fn range(&self) -> (u32, u32) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
}

/// On-screen width of a new text box (px) and its font size (px per em).
const NEW_BOX_PX: f64 = 600.0;

/// A tap with the text tool at `at` (canvas): selects the text box there (topmost; tapping it
/// again types into it, dragging moves it), or starts a new box; ends the previous edit.
pub(crate) fn tap(s: &mut Session, note: vibenote_model::NoteId, at: (f64, f64)) -> Result<(), EngineError> {
    let p = Point::new(at.0, at.1);
    let double = s.live.double;
    let canvas = s.ws.canvas(note)?;
    let hit = canvas
        .objects
        .values()
        .filter(|o| matches!(o.kind, ObjectKind::Text { .. }))
        .filter(|o| s.object_box(o).is_some_and(|b| b.inflate(4.0 / s.zoom()).contains(p)))
        .max_by_key(|o| (o.z, o.id))
        .map(|o| o.id.0);
    // In the box being typed into, a click moves the caret and a double click selects the
    // word (like any text field).
    if let Some(ed) = s.text_edit.as_mut()
        && ed.note == note
        && ed.id.is_some()
        && ed.id == hit
    {
        ed.place = Some(at);
        ed.word = double;
        s.coalesce = None;
        return Ok(());
    }
    end(s)?;
    if let Some(id) = hit
        && double
    {
        return edit(s, note, id, at, true);
    }
    if let Some(id) = hit {
        s.selection = crate::Selection {
            note: Some(note),
            ids: vec![id],
            last: None,
        };
        return Ok(());
    }
    #[allow(clippy::cast_possible_truncation)] // a font size in canvas units
    let size = (f64::from(vibenote_model::default_text_size()) / s.zoom()) as f32;
    s.text_edit = Some(TextEdit {
        note,
        id: None,
        // The tap is the middle of the first line.
        origin: Point::new(p.x, p.y - f64::from(size) * 0.65),
        width: NEW_BOX_PX / s.zoom(),
        size,
        caret: 0,
        anchor: 0,
        place: None,
        word: false,
        line: false,
        place_from: None,
        extend: false,
        // A new box has nothing to move around in: typing starts right away.
        vim: s
            .settings
            .vim
            .then(|| super::vim::VimState::new(super::vim::VimMode::Insert)),
    });
    Ok(())
}

/// A press is this close to the box being edited (screen px) but outside its text: it grabs
/// the frame to move the box (R-TEXT-005).
const FRAME_BAND_PX: f64 = 8.0;

/// What a press with the text tool did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Not on a text box.
    Missed,
    /// Into a box's text: the caret goes there, a drag selects.
    Text,
    /// On the frame of the box being edited: dragging moves it.
    Frame,
}

/// The text area of a box's frame (a vim-mode frame has padding around it).
fn text_area(s: &Session, fr: vibenote_geom::Rect, size: f32) -> vibenote_geom::Rect {
    if s.settings.vim {
        let pad = crate::vim_pad(size);
        vibenote_geom::Rect {
            min: Point::new(fr.min.x + pad, fr.min.y + pad),
            max: Point::new(fr.max.x - pad, fr.max.y - pad),
        }
    } else {
        fr
    }
}

/// A press at `at` with the text tool (or a mouse click with an ink tool): like an ordinary
/// text field (R-TEXT-005). Into a box's text it starts editing with the caret there (a
/// double press selects the word, a triple press the line; `extend` keeps the selection's
/// other end); on the frame band of the box being edited it starts moving the box.
pub(crate) fn press(
    s: &mut Session,
    note: vibenote_model::NoteId,
    at: (f64, f64),
    extend: bool,
    mouse: bool,
) -> Result<Press, EngineError> {
    let p = Point::new(at.0, at.1);
    let band = FRAME_BAND_PX / s.zoom();
    let editing = s.text_edit.as_ref().filter(|e| e.note == note).and_then(|e| e.id);
    let canvas = s.ws.canvas(note)?;
    if let Some(id) = editing
        && let Some(o) = canvas.objects.get(&ObjectId(id))
        && let ObjectKind::Text { size, .. } = o.kind
        && let Some(fr) = s.object_box(o)
        && fr.inflate(band).contains(p)
        && !text_area(s, fr, size).contains(p)
    {
        s.selection = crate::Selection {
            note: Some(note),
            ids: vec![id],
            last: None,
        };
        return Ok(if s.begin_move(at, mouse) {
            Press::Frame
        } else {
            Press::Missed
        });
    }
    let hit = canvas
        .objects
        .values()
        .filter(|o| matches!(o.kind, ObjectKind::Text { .. }))
        .filter(|o| s.object_box(o).is_some_and(|b| b.inflate(4.0 / s.zoom()).contains(p)))
        .max_by_key(|o| (o.z, o.id))
        .map(|o| o.id.0);
    let Some(id) = hit else { return Ok(Press::Missed) };
    let clicks = s.live.clicks;
    if editing != Some(id) {
        edit(s, note, id, at, false)?;
    }
    if let Some(ed) = s.text_edit.as_mut() {
        ed.place = Some(at);
        ed.word = clicks == 2;
        ed.line = clicks >= 3;
        ed.place_from = None;
        ed.extend = extend && editing == Some(id);
    }
    s.selection = crate::Selection::default();
    s.live.text_drag = Some((at, mouse));
    s.coalesce = None;
    Ok(Press::Text)
}

/// The pointer moved while pressing into a box's text: the selection follows it.
pub(crate) fn drag_to(s: &mut Session, at: (f64, f64)) {
    let Some((from, _)) = s.live.text_drag else { return };
    if let Some(ed) = s.text_edit.as_mut() {
        ed.place_from = Some(from);
        ed.place = Some(at);
        ed.word = false;
        ed.line = false;
    }
}

/// Starts typing into text box `id` with the caret at canvas point `at` (the UI places it with
/// the layout); the box stops being selected.
pub(crate) fn edit(
    s: &mut Session,
    note: vibenote_model::NoteId,
    id: u64,
    at: (f64, f64),
    word: bool,
) -> Result<(), EngineError> {
    end(s)?;
    let o = super::pdf::object(s, note, id)?;
    let ObjectKind::Text {
        origin,
        width,
        size,
        content,
        ..
    } = &o.kind
    else {
        return Err(EngineError::Model(format!("object {id} is not a text box")));
    };
    let end = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
    s.selection = crate::Selection::default();
    s.text_edit = Some(TextEdit {
        note,
        id: Some(id),
        origin: *origin,
        width: *width,
        size: *size,
        caret: end,
        anchor: end,
        place: Some(at),
        word,
        line: false,
        place_from: None,
        extend: false,
        vim: s
            .settings
            .vim
            .then(|| super::vim::VimState::new(super::vim::VimMode::Normal)),
    });
    Ok(())
}

/// Starts typing into text box `id` with the caret at character `offset` (the finder, a
/// keyboard jump); vim mode starts in Normal mode on that character.
pub(crate) fn open_at(s: &mut Session, note: vibenote_model::NoteId, id: u64, offset: u32) -> Result<(), EngineError> {
    edit(s, note, id, (0.0, 0.0), false)?;
    let len = u32::try_from(
        content_of(
            edited(
                s,
                s.text_edit
                    .as_ref()
                    .ok_or_else(|| EngineError::Internal("no edit".into()))?,
            )
            .as_ref(),
        )
        .chars()
        .count(),
    )
    .unwrap_or(u32::MAX);
    if let Some(ed) = s.text_edit.as_mut() {
        ed.place = None;
        let max = if ed.vim.is_some() { len.saturating_sub(1) } else { len };
        ed.caret = offset.min(max);
        ed.anchor = ed.caret;
    }
    Ok(())
}

/// Sets a text box's wrap width (one undo step).
pub(crate) fn set_width(s: &mut Session, note: vibenote_model::NoteId, id: u64, w: f64) -> Result<(), EngineError> {
    if !(w.is_finite() && w > 0.0 && w <= vibenote_model::limits::MAX_COORD) {
        return Err(EngineError::Model("text width out of range".into()));
    }
    let mut o = super::pdf::object(s, note, id)?;
    if let ObjectKind::Text { width, .. } = &mut o.kind {
        *width = w;
    }
    s.commit(
        Scope::Note(note),
        "text width",
        vec![Op::ReplaceObject { note, after: o }],
    )
}

/// Ends editing; a box left empty is removed (its own undo step).
pub(crate) fn end(s: &mut Session) -> Result<(), EngineError> {
    s.coalesce = None;
    let Some(ed) = s.text_edit.take() else { return Ok(()) };
    let Some(id) = ed.id else { return Ok(()) };
    let Ok(o) = super::pdf::object(s, ed.note, id) else {
        return Ok(()); // gone (undone)
    };
    if matches!(&o.kind, ObjectKind::Text { content, .. } if content.is_empty()) {
        s.commit(
            Scope::Note(ed.note),
            "remove empty text",
            vec![Op::RemoveObject {
                note: ed.note,
                object: o,
            }],
        )?;
    }
    Ok(())
}

/// The edited box's content, or `None` if there is none (yet, or any more).
pub(crate) fn edited(s: &Session, ed: &TextEdit) -> Option<CanvasObject> {
    let id = ed.id?;
    let o = s.ws.canvas(ed.note).ok()?.objects.get(&ObjectId(id))?.clone();
    matches!(o.kind, ObjectKind::Text { .. }).then_some(o)
}

pub(crate) fn content_of(o: Option<&CanvasObject>) -> &str {
    match o.map(|o| &o.kind) {
        Some(ObjectKind::Text { content, .. }) => content,
        _ => "",
    }
}

/// Replaces the selection by `text` (typing, Enter, Backspace with a selection).
pub(crate) fn type_text(s: &mut Session, mut ed: TextEdit, text: &str) -> Result<TextEdit, EngineError> {
    let (a, b) = ed.range();
    let k = u32::try_from(text.chars().count()).map_err(|_| EngineError::Model("text too long".into()))?;
    match edited(s, &ed) {
        Some(mut o) => {
            let ObjectKind::Text { content, marks, .. } = &mut o.kind else {
                return Ok(ed);
            };
            let len = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
            let (a, b) = (a.min(len), b.min(len));
            splice(
                content,
                marks,
                (a < b).then_some([a, b]),
                (!text.is_empty()).then_some(text),
                Some(a),
            )?;
            // Typed text continues the formatting it follows.
            if k > 0 {
                for m in marks.iter_mut() {
                    if m.end == a && m.start < a {
                        m.end = m.end.saturating_add(k);
                    }
                }
                normalise(marks);
            }
            let id = o.id.0;
            s.commit_typing(
                ed.note,
                id,
                vec![Op::ReplaceObject {
                    note: ed.note,
                    after: o,
                }],
            )?;
            ed.caret = a.saturating_add(k);
        }
        None if !text.is_empty() => {
            let z = s.next_z(ed.note)?;
            let id = ObjectId(s.ws.alloc_id()?);
            let object = CanvasObject {
                id,
                z,
                kind: ObjectKind::Text {
                    origin: ed.origin,
                    width: ed.width,
                    content: text.to_string(),
                    marks: Vec::new(),
                    size: ed.size,
                },
            };
            s.commit_typing(ed.note, id.0, vec![Op::InsertObject { note: ed.note, object }])?;
            ed.id = Some(id.0);
            ed.caret = k;
        }
        None => {}
    }
    ed.anchor = ed.caret;
    Ok(ed)
}

/// The word (letters, digits, `_`) at or just before character offset `c`, as `[start, end)`;
/// on anything else just that character.
fn word_at(t: &str, c: u32) -> (u32, u32) {
    let chars: Vec<char> = t.chars().collect();
    let is_word = |i: usize| chars.get(i).is_some_and(|ch| ch.is_alphanumeric() || *ch == '_');
    let mut i = usize::try_from(c).unwrap_or(usize::MAX).min(chars.len());
    if !is_word(i) && i > 0 && is_word(i.saturating_sub(1)) {
        i = i.saturating_sub(1);
    }
    let to = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    if !is_word(i) {
        return (to(i), to(i.saturating_add(1).min(chars.len())));
    }
    let mut a = i;
    while a > 0 && is_word(a.saturating_sub(1)) {
        a = a.saturating_sub(1);
    }
    let mut b = i;
    while is_word(b) {
        b = b.saturating_add(1);
    }
    (to(a), to(b))
}

/// The line at character `c`: `[start, end)` without its line break.
fn line_at(t: &str, c: u32) -> (u32, u32) {
    let chars: Vec<char> = t.chars().collect();
    let i = usize::try_from(c).unwrap_or(usize::MAX).min(chars.len());
    let a = chars
        .get(..i)
        .and_then(|h| h.iter().rposition(|x| *x == '\n'))
        .map_or(0, |k| k.saturating_add(1));
    let b = chars
        .get(i..)
        .and_then(|r| r.iter().position(|x| *x == '\n'))
        .map_or(chars.len(), |k| i.saturating_add(k));
    let to = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    (to(a), to(b))
}

/// Character offsets where the lines of `t` start.
fn line_starts(t: &str) -> Vec<u32> {
    let mut v = vec![0u32];
    for (i, c) in t.chars().enumerate() {
        if c == '\n' {
            v.push(u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1));
        }
    }
    v
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeyP {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    SelectAll,
    /// Stop editing (Escape).
    Escape,
    /// Indent: spaces to the next tab stop (with `shift`: one indent level less).
    Tab,
}

/// Spaces per indent level (Tab, `>>`, vim's shiftwidth).
pub(crate) const INDENT: usize = 4;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InputP {
    /// Typed text (inserted at the caret, replacing the selection).
    text: Option<String>,
    key: Option<KeyP>,
    /// Shift held: arrows and Home/End extend the selection.
    #[serde(default)]
    shift: bool,
    /// Ctrl held with `text` (vim mode: Ctrl+R redo).
    #[serde(default)]
    ctrl: bool,
    /// When the key was pressed (ms; vim mode's `jk` and leader timing).
    t_ms: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CaretP {
    offset: u32,
    /// Select the word at `offset` (a double click).
    #[serde(default)]
    word: bool,
    /// Keep the anchor (Shift+click extends the selection).
    #[serde(default)]
    extend: bool,
    /// The other end of the selection (a drag from there to `offset`).
    anchor: Option<u32>,
    /// Select the line at `offset` (a triple click).
    #[serde(default)]
    line: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StyleP {
    format: FormatP,
    value: Option<f64>,
}

/// Most characters typed in one `text.input` (paste comes later through the clipboard).
const MAX_TYPED: usize = 1 << 20;

fn editing(s: &Session) -> Result<TextEdit, EngineError> {
    s.text_edit
        .clone()
        .ok_or_else(|| EngineError::Model("no text box is being edited".into()))
}

pub(crate) fn key(s: &mut Session, mut ed: TextEdit, k: KeyP, shift: bool) -> Result<Option<TextEdit>, EngineError> {
    let o = edited(s, &ed);
    let text = content_of(o.as_ref()).to_string();
    let len = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
    ed.caret = ed.caret.min(len);
    ed.anchor = ed.anchor.min(len);
    let (a, b) = ed.range();
    let starts = line_starts(&text);
    let line = starts.partition_point(|&st| st <= ed.caret).saturating_sub(1);
    let line_start = starts.get(line).copied().unwrap_or(0);
    let line_end = starts.get(line.saturating_add(1)).map_or(len, |n| n.saturating_sub(1));
    let col = ed.caret.saturating_sub(line_start);
    let moved = |ed: &mut TextEdit, to: u32| {
        ed.caret = to.min(len);
        if !shift {
            ed.anchor = ed.caret;
        }
    };
    match k {
        KeyP::Escape => return Ok(None),
        KeyP::Enter => return type_text(s, ed, "\n").map(Some),
        KeyP::Backspace | KeyP::Delete if a < b => return type_text(s, ed, "").map(Some),
        KeyP::Backspace if a > 0 => {
            ed.anchor = a.saturating_sub(1);
            ed.caret = a;
            return type_text(s, ed, "").map(Some);
        }
        KeyP::Delete if a < len => {
            ed.anchor = a;
            ed.caret = a.saturating_add(1);
            return type_text(s, ed, "").map(Some);
        }
        KeyP::Backspace | KeyP::Delete => {}
        KeyP::Tab if shift => return dedent(s, ed).map(Some),
        KeyP::Tab => {
            let col = usize::try_from(col).unwrap_or(0);
            let n = INDENT.saturating_sub(col.checked_rem(INDENT).unwrap_or(0)).max(1);
            return type_text(s, ed, &" ".repeat(n)).map(Some);
        }
        KeyP::Left if !shift && a < b => moved(&mut ed, a),
        KeyP::Right if !shift && a < b => moved(&mut ed, b),
        KeyP::Left => {
            let to = ed.caret.saturating_sub(1);
            moved(&mut ed, to);
        }
        KeyP::Right => {
            let to = ed.caret.saturating_add(1);
            moved(&mut ed, to);
        }
        KeyP::Home => moved(&mut ed, line_start),
        KeyP::End => moved(&mut ed, line_end),
        KeyP::Up if line == 0 => moved(&mut ed, 0),
        KeyP::Up => {
            let ps = starts.get(line.saturating_sub(1)).copied().unwrap_or(0);
            moved(&mut ed, ps.saturating_add(col).min(line_start.saturating_sub(1)));
        }
        KeyP::Down => match starts.get(line.saturating_add(1)) {
            Some(&ns) => {
                let ne = starts.get(line.saturating_add(2)).map_or(len, |n| n.saturating_sub(1));
                moved(&mut ed, ns.saturating_add(col).min(ne));
            }
            None => moved(&mut ed, len),
        },
        KeyP::SelectAll => {
            ed.anchor = 0;
            ed.caret = len;
        }
    }
    // Moving the caret starts a new undo step for the next typing.
    s.coalesce = None;
    Ok(Some(ed))
}

/// Shift+Tab: removes one indent level (up to [`INDENT`] leading spaces) from the caret's line.
pub(crate) fn dedent(s: &mut Session, mut ed: TextEdit) -> Result<TextEdit, EngineError> {
    let text: Vec<char> = content_of(edited(s, &ed).as_ref()).chars().collect();
    let caret = usize::try_from(ed.caret).unwrap_or(0).min(text.len());
    let ls = text
        .get(..caret)
        .and_then(|h| h.iter().rposition(|c| *c == '\n'))
        .map_or(0, |i| i.saturating_add(1));
    let k = (0..INDENT)
        .take_while(|&i| text.get(ls.saturating_add(i)) == Some(&' '))
        .count();
    if k == 0 {
        return Ok(ed);
    }
    let to = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    ed.anchor = to(ls);
    ed.caret = to(ls.saturating_add(k));
    let mut ed = type_text(s, ed, "")?;
    let back = caret.saturating_sub(k).max(ls);
    ed.caret = to(back);
    ed.anchor = ed.caret;
    Ok(ed)
}

pub(crate) fn register_editing(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.input",
            doc: "Type into the text box being edited: `text` replaces the selection at the caret; `key` is backspace, delete, left, right, up, down, home, end, enter, tab (indent; with `shift` dedent), select_all or escape (stop editing); `shift` extends the selection. Typing into one box is one undo step until the caret moves. In vim mode keys are vim's (`ctrl` with `text` for Ctrl+R, `t_ms` for jk and leader timing).",
            reqs: &["R-TEXT-001", "R-TEXT-002", "R-VIM-002", "R-VIM-003", "R-VIM-004"],
            implemented: true,
        },
        |s: &mut Session, p: InputP| {
            let mut ed = editing(s)?;
            let text = match &p.text {
                Some(t) if t.chars().count() > MAX_TYPED => {
                    return Err(EngineError::Model("too much text at once".into()));
                }
                // Control characters other than newlines and tabs are not text.
                Some(t) => Some(t.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect::<String>()),
                None => None,
            };
            let next = if ed.vim.is_some() && s.settings.vim {
                super::vim::input(s, ed, text.as_deref(), p.key, p.shift, p.ctrl, p.t_ms)?
            } else {
                ed.vim = None;
                if let Some(t) = &text {
                    ed = type_text(s, ed, t)?;
                }
                match p.key {
                    Some(k) => key(s, ed, k, p.shift)?,
                    None => Some(ed),
                }
            };
            match next {
                Some(ed) => s.text_edit = Some(ed),
                None => end(s)?,
            }
            Ok(s.text_edit.clone())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.caret",
            doc: "Put the caret of the text box being edited at a character offset (`extend`: keep the selection's other end).",
            reqs: &["R-TEXT-001"],
            implemented: true,
        },
        |s: &mut Session, p: CaretP| {
            let mut ed = editing(s)?;
            let content = content_of(edited(s, &ed).as_ref()).to_string();
            let len = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
            let off = p.offset.min(len);
            // The selection as [anchor, caret) like a text field.
            let (mut a, mut c) = match p.anchor {
                Some(x) => (x.min(len), off),
                None if p.extend => (ed.anchor.min(len), off),
                None => (off, off),
            };
            if p.word {
                (a, c) = word_at(&content, off);
            }
            if p.line {
                (a, c) = line_at(&content, off);
            }
            match ed.vim.is_some() {
                // Vim: a click is the Normal-mode cursor, a selection Visual mode (R-VIM-001).
                true => super::vim::click(&mut ed, &content, a, c, p.line),
                false => {
                    ed.anchor = a;
                    ed.caret = c;
                }
            }
            ed.place = None;
            ed.place_from = None;
            ed.word = false;
            ed.line = false;
            ed.extend = false;
            s.coalesce = None;
            s.text_edit = Some(ed.clone());
            Ok(ed)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.style",
            doc: "Toggle a format on the selection of the text box being edited (or the whole box when nothing is selected): bold, italic, underline, strike, heading1–3, bullet_list, numbered_list, checkbox; color/size set their value. One undo step.",
            reqs: &["R-TEXT-001"],
            implemented: true,
        },
        |s: &mut Session, p: StyleP| {
            let ed = editing(s)?;
            let Some(mut o) = edited(s, &ed) else {
                return Ok(());
            };
            let ObjectKind::Text { content, marks, .. } = &mut o.kind else {
                return Ok(());
            };
            let len = u32::try_from(content.chars().count()).unwrap_or(u32::MAX);
            let (a, b) = match ed.range() {
                (a, b) if a < b => (a.min(len), b.min(len)),
                _ => (0, len),
            };
            if a >= b {
                return Ok(());
            }
            let style = style_of(p.format, p.value, false)?;
            let kind = std::mem::discriminant(&style);
            // Toggle: a format already over the whole range comes off.
            let covered = |m: &&TextMark| std::mem::discriminant(&m.style) == kind && m.start <= a && m.end >= b;
            let on = marks.iter().find(covered).is_some_and(|m| m.style == style);
            clear(marks, kind, a, b);
            if !on {
                marks.push(TextMark { start: a, end: b, style });
            }
            normalise(marks);
            s.commit(Scope::Note(ed.note), "format text", vec![Op::ReplaceObject { note: ed.note, after: o }])?;
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.end",
            doc: "Stop editing text (a box left empty is removed).",
            reqs: &["R-TEXT-001"],
            implemented: true,
        },
        |s: &mut Session, _: EndP| end(s),
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "text.open",
            doc: "Start typing into a text box with the caret at a character offset (no pointer needed); in vim mode in Normal mode.",
            reqs: &["R-TEXT-001", "R-VIM-001", "R-VIM-005"],
            implemented: true,
        },
        |s: &mut Session, p: OpenP| {
            let (note, _) = text_object(s, p.note, p.id)?;
            open_at(s, note, p.id, p.offset)?;
            Ok(s.text_edit.clone())
        },
    )))?;
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EndP {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenP {
    id: u64,
    /// Caret position (characters; default 0).
    #[serde(default)]
    offset: u32,
    note: Option<u64>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn mark(start: u32, end: u32, style: TextStyle) -> TextMark {
        TextMark { start, end, style }
    }

    #[test]
    fn marks_merge_and_clear_splits() {
        let mut m = vec![
            mark(0, 4, TextStyle::Bold),
            mark(4, 8, TextStyle::Bold),
            mark(2, 3, TextStyle::Italic),
        ];
        normalise(&mut m);
        assert_eq!(m, vec![mark(0, 8, TextStyle::Bold), mark(2, 3, TextStyle::Italic)]);
        clear(&mut m, std::mem::discriminant(&TextStyle::Bold), 3, 5);
        normalise(&mut m);
        assert_eq!(
            m,
            vec![
                mark(0, 3, TextStyle::Bold),
                mark(2, 3, TextStyle::Italic),
                mark(5, 8, TextStyle::Bold)
            ]
        );
        assert_eq!(after_delete(10, 2, 5), 7);
        assert_eq!(after_delete(3, 2, 5), 2);
        assert_eq!(byte_at("Äb", 1), 2);
    }
}
