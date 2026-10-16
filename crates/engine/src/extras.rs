//! Text boxes, link labels and shapes of the view, for the renderer (R-TEXT-001, R-LINK-001,
//! R-SHAPE-001). Drawn above PDF pages and images, below ink.

use vibenote_geom::{Point, Rect};
use vibenote_model::{Color, LinkTarget, ObjectKind, Sample, Stroke, TextMark, TextStyle, Tool, shape_outline};
use vibenote_render::TextItem;

/// Link label colour and size.
const LINK_COLOR: Color = Color(0x2a6f_dbff);
/// Audio players on the canvas.
const AUDIO_COLOR: Color = Color(0x5a5f_6aff);
/// The progress bar's track.
const AUDIO_TRACK_COLOR: Color = Color(0xb9be_c8ff);
const LINK_SIZE: f32 = 14.0;

fn key_of(parts: &[&[u8]]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in parts {
        for b in *p {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h = (h ^ 0xff).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// With vim mode on, text boxes wrap at no fewer than this many columns of the box's base size
/// (an editor's width, R-VIM-001).
pub const VIM_COLUMNS: f64 = 100.0;
/// JetBrains Mono's advance: 600 units per 1000-unit em.
const MONO_ADVANCE: f64 = 0.6;
/// Padding of a box's frame with vim mode on, in ems of the box's base size (R-VIM-001).
const VIM_PAD_EM: f64 = 0.75;

/// The frame padding of a vim-mode text box (canvas units).
pub fn vim_pad(size: f32) -> f64 {
    VIM_PAD_EM * f64::from(size.max(1.0))
}

/// A text box as the renderer draws it (same key, same layout). With vim mode on (`mono`) it
/// wraps at least at [`VIM_COLUMNS`].
fn text_item(origin: Point, width: f64, size: f32, content: String, marks: Vec<TextMark>, mono: bool) -> TextItem {
    let width = if mono {
        width.max(VIM_COLUMNS * MONO_ADVANCE * f64::from(size.max(1.0)))
    } else {
        width
    };
    let marks_json = serde_json::to_vec(&marks).unwrap_or_default();
    TextItem {
        key: key_of(&[
            content.as_bytes(),
            &marks_json,
            &width.to_bits().to_le_bytes(),
            &size.to_bits().to_le_bytes(),
            &[u8::from(mono)],
        ]),
        origin,
        width,
        size,
        content,
        marks,
        color: Color::INK,
        mono,
    }
}

/// The frame of a laid-out text item: its origin, the widest line (at least one em, at most
/// the wrap width) and all lines (R-TEXT-004). With vim mode on (`mono`), the whole wrap width
/// and padding around it, like an editor (R-VIM-001).
pub fn frame_of(item: &TextItem) -> Rect {
    let (w, h) = vibenote_render::text_measure(item);
    if item.mono {
        let pad = vim_pad(item.size);
        return Rect {
            min: Point::new(item.origin.x - pad, item.origin.y - pad),
            max: Point::new(item.origin.x + item.width.max(1.0) + pad, item.origin.y + h + pad),
        };
    }
    let w = w.max(f64::from(item.size.max(1.0))).min(item.width.max(1.0));
    Rect {
        min: item.origin,
        max: Point::new(item.origin.x + w, item.origin.y + h),
    }
}

fn meets(a: &Rect, b: &Rect) -> bool {
    a.min.x <= b.max.x && a.max.x >= b.min.x && a.min.y <= b.max.y && a.max.y >= b.min.y
}

impl crate::Session {
    /// The label a link shows.
    pub fn link_label(&self, t: &LinkTarget) -> String {
        match t {
            LinkTarget::Note { note, .. } => self
                .ws
                .notes
                .get(note)
                .map_or_else(|| "(deleted note)".to_string(), |n| format!("→ {}", n.name)),
            LinkTarget::Folder { folder } => self
                .ws
                .folders
                .get(folder)
                .map_or_else(|| "(deleted folder)".to_string(), |f| format!("→ {}/", f.name)),
            LinkTarget::Url { url } => url.clone(),
        }
    }

    /// The box an object covers on the canvas: text boxes and link labels by their layout
    /// estimate, everything else by its geometry.
    pub fn object_box(&self, o: &vibenote_model::CanvasObject) -> Option<Rect> {
        match &o.kind {
            ObjectKind::Text {
                origin,
                width,
                size,
                content,
                marks,
            } => Some(frame_of(&text_item(
                *origin,
                *width,
                *size,
                content.clone(),
                marks.clone(),
                self.settings.vim,
            ))),
            ObjectKind::Link { origin, target } => Some(self.link_rect(*origin, target).0),
            k => k.bounds(),
        }
    }

    /// A link label's box and its character count.
    fn link_rect(&self, origin: Point, target: &LinkTarget) -> (Rect, u32) {
        let label = self.link_label(target);
        let n = u32::try_from(label.chars().count()).unwrap_or(u32::MAX);
        #[allow(clippy::cast_precision_loss)] // a label width estimate
        let width = f64::from(n.max(1)) * 0.62 * f64::from(LINK_SIZE) + 8.0;
        let r = Rect {
            min: origin,
            max: Point::new(origin.x + width, origin.y + 2.0 * f64::from(LINK_SIZE)),
        };
        (r, n)
    }

    /// The text box being edited as the renderer lays it out (an empty one before the first
    /// character), for the caret and selection (R-TEXT-001).
    pub fn edited_text_item(&self) -> Option<TextItem> {
        let ed = self.text_edit.as_ref()?;
        let o = ed.id.and_then(|id| {
            let o = self
                .ws
                .canvas(ed.note)
                .ok()?
                .objects
                .get(&vibenote_model::ObjectId(id))?;
            // While its frame is dragged, where it would land.
            Some(self.drag_preview(ed.note, o).unwrap_or_else(|| o.clone()))
        });
        let (origin, width, size, content, marks) = match o.map(|o| o.kind) {
            Some(ObjectKind::Text {
                origin,
                width,
                size,
                content,
                marks,
            }) => (origin, width, size, content, marks),
            _ => (ed.origin, ed.width, ed.size, String::new(), Vec::new()),
        };
        Some(text_item(origin, width, size, content, marks, self.settings.vim))
    }

    /// Text items and vector strokes of the current note in a view (`origin`, `zoom`, surface
    /// size in px), in z order.
    pub fn extras_in_view(&self, origin: (f64, f64), zoom: f64, size: (f64, f64)) -> (Vec<TextItem>, Vec<Stroke>) {
        let mut texts = Vec::new();
        let mut vectors = Vec::new();
        let Some(note) = self.current_note else {
            return (texts, vectors);
        };
        let Ok(canvas) = self.ws.canvas(note) else {
            return (texts, vectors);
        };
        if !(zoom.is_finite() && zoom > 0.0) {
            return (texts, vectors);
        }
        let view = Rect {
            min: Point::new(origin.0, origin.1),
            max: Point::new(origin.0 + size.0 / zoom, origin.1 + size.1 / zoom),
        };
        for id in self.non_stroke_ids(note, canvas) {
            let Some(o) = canvas.objects.get(&id) else { continue };
            // A selection being dragged shows where it would land (R-PEN-009).
            let moved = self.drag_preview(note, o);
            let o = moved.as_ref().unwrap_or(o);
            match &o.kind {
                ObjectKind::Text {
                    origin,
                    width,
                    content,
                    marks,
                    size,
                } => {
                    let item = text_item(
                        *origin,
                        *width,
                        *size,
                        content.clone(),
                        marks.clone(),
                        self.settings.vim,
                    );
                    if !meets(&frame_of(&item), &view) {
                        continue;
                    }
                    texts.push(item);
                }
                ObjectKind::Link { origin, target } => {
                    let label = self.link_label(target);
                    let (r, n) = self.link_rect(*origin, target);
                    let width = r.width();
                    if !meets(&r, &view) || n == 0 {
                        continue;
                    }
                    texts.push(TextItem {
                        key: key_of(&[b"link", label.as_bytes()]),
                        origin: *origin,
                        width,
                        size: LINK_SIZE,
                        marks: vec![TextMark {
                            start: 0,
                            end: n,
                            style: TextStyle::Underline,
                        }],
                        content: label,
                        color: LINK_COLOR,
                        mono: false,
                    });
                }
                ObjectKind::Shape {
                    shape,
                    points,
                    closed,
                    color,
                    width,
                } => {
                    if !o.kind.bounds().is_some_and(|b| meets(&b, &view)) {
                        continue;
                    }
                    for line in shape_outline(*shape, points, *closed, *width) {
                        vectors.push(Stroke {
                            tool: Tool::Pen,
                            color: *color,
                            width: *width,
                            samples: line
                                .into_iter()
                                .map(|p| Sample {
                                    x: p.x,
                                    y: p.y,
                                    pressure: 1.0,
                                    tilt_x: 0.0,
                                    tilt_y: 0.0,
                                    t_us: 0,
                                })
                                .collect(),
                        });
                    }
                }
                ObjectKind::Audio {
                    origin,
                    duration_ms,
                    recorded,
                    ..
                } => {
                    let Some(b) = o.kind.bounds().filter(|b| meets(b, &view)) else {
                        continue;
                    };
                    // A player (R-AUDIO-004): a box, the play/pause button on the left, the
                    // progress bar and the position on the right.
                    let pt = |x: f64, y: f64| Sample {
                        x,
                        y,
                        pressure: 1.0,
                        tilt_x: 0.0,
                        tilt_y: 0.0,
                        t_us: 0,
                    };
                    let line = |pts: Vec<Sample>, width: f32, color: Color| Stroke {
                        tool: Tool::Pen,
                        color,
                        width,
                        samples: pts,
                    };
                    let (x0, y0, x1, y1) = (b.min.x, b.min.y, b.max.x, b.max.y);
                    let h = y1 - y0;
                    vectors.push(line(
                        vec![pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1), pt(x0, y0)],
                        1.5,
                        AUDIO_COLOR,
                    ));
                    let state = self.audio.player(o.id);
                    let cx = x0 + 0.5 * crate::commands::audio::button_width(&b);
                    if state.is_some_and(|v| v.playing) {
                        for dx in [-0.1, 0.1] {
                            vectors.push(line(
                                vec![pt(cx + dx * h, y0 + 0.3 * h), pt(cx + dx * h, y0 + 0.7 * h)],
                                3.0,
                                AUDIO_COLOR,
                            ));
                        }
                    } else {
                        vectors.push(line(
                            vec![
                                pt(cx - 0.12 * h, y0 + 0.3 * h),
                                pt(cx + 0.18 * h, y0 + 0.5 * h),
                                pt(cx - 0.12 * h, y0 + 0.7 * h),
                                pt(cx - 0.12 * h, y0 + 0.3 * h),
                            ],
                            1.5,
                            AUDIO_COLOR,
                        ));
                    }
                    let bar0 = x0 + crate::commands::audio::button_width(&b);
                    let bar1 = (x1 - 0.25 * h).max(bar0);
                    let by = y0 + 0.75 * h;
                    vectors.push(line(vec![pt(bar0, by), pt(bar1, by)], 1.0, AUDIO_TRACK_COLOR));
                    let at = state.map_or(0, |v| v.position_ms);
                    if at > 0 && *duration_ms > 0 {
                        #[allow(clippy::cast_precision_loss)] // a fraction for drawing
                        let f = (at as f64 / *duration_ms as f64).clamp(0.0, 1.0);
                        vectors.push(line(
                            vec![pt(bar0, by), pt(bar0 + f * (bar1 - bar0), by)],
                            3.0,
                            LINK_COLOR,
                        ));
                    }
                    let mmss = |ms: u64| format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60);
                    let label = match state {
                        Some(_) => format!("{} / {}", mmss(at), mmss(*duration_ms)),
                        None => format!("{}{}", if *recorded { "● " } else { "" }, mmss(*duration_ms)),
                    };
                    texts.push(TextItem {
                        key: key_of(&[b"audio", label.as_bytes()]),
                        origin: Point::new(bar0, origin.y + 0.12 * h),
                        width: (x1 - bar0).max(1.0),
                        size: LINK_SIZE,
                        marks: Vec::new(),
                        content: label,
                        color: AUDIO_COLOR,
                        mono: false,
                    });
                }
                _ => {}
            }
        }
        (texts, vectors)
    }
}
