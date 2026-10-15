use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use vibenote_geom::{Point, Rect};

macro_rules! id_type {
    ($($name:ident),*) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
        }
    )*};
}
id_type!(FolderId, NoteId, ObjectId);

/// sRGB colour, 0xRRGGBBAA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Color(pub u32);

impl Color {
    pub const INK: Color = Color(0x1e1714ff);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub parent: Option<FolderId>,
    pub name: String,
    pub color: Option<Color>,
    /// Manual order among siblings (R-ORG-002).
    pub sort: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    pub folder: Option<FolderId>,
    pub name: String,
    pub color: Option<Color>,
    pub sort: i64,
    /// Background of this note (R-CANVAS-006); `None` uses the workspace default from settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<crate::settings::Background>,
}

/// One pen sample, kept exactly as received (R-PEN-004).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Canvas units.
    pub x: f64,
    pub y: f64,
    /// 0..1
    pub pressure: f32,
    /// Degrees, −90..90.
    pub tilt_x: f32,
    pub tilt_y: f32,
    /// Microseconds since the stroke's first sample.
    pub t_us: u64,
}

/// Quantisation grid for stored samples (exact save→load identity, ADR 0005).
pub mod grid {
    /// Coordinates: 1/1024 canvas unit (far below display resolution even at 64× zoom).
    pub const XY: f64 = 1024.0;
    /// Pressure: 1/65535.
    pub const P: f32 = 65535.0;
    /// Tilt: 1/100 degree.
    pub const TILT: f32 = 100.0;
}

impl Sample {
    pub fn point(&self) -> Point {
        Point::new(self.x, self.y)
    }

    /// Snaps the sample to the storage grid. Idempotent. Uses exactly the arithmetic of the
    /// storage decoder (`k / grid`), so encode∘decode is the identity on quantised samples.
    pub fn quantised(self) -> Sample {
        let qx = (self.x * grid::XY).round() / grid::XY;
        let qy = (self.y * grid::XY).round() / grid::XY;
        let qp = (self.pressure * grid::P).round() / grid::P;
        let qtx = (self.tilt_x * grid::TILT).round() / grid::TILT;
        let qty = (self.tilt_y * grid::TILT).round() / grid::TILT;
        Sample {
            x: qx,
            y: qy,
            pressure: qp,
            tilt_x: qtx,
            tilt_y: qty,
            t_us: self.t_us,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Pen,
    Highlighter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub tool: Tool,
    pub color: Color,
    /// Base width in canvas units at pressure 1.
    pub width: f32,
    pub samples: Vec<Sample>,
}

impl Stroke {
    /// The stroke's tool, colour and width with no samples (a template for pieces).
    pub fn clone_header(&self) -> Stroke {
        Stroke {
            tool: self.tool,
            color: self.color,
            width: self.width,
            samples: Vec::new(),
        }
    }

    /// Box of all the stroke's ink: its samples grown by 1.25 widths (a fully tilted
    /// highlighter at full pressure has a radius of 1.24 widths; the pen 0.5).
    pub fn bounds(&self) -> Rect {
        let mut r = Rect::EMPTY;
        for s in &self.samples {
            r.include(s.point());
        }
        r.inflate(f64::from(self.width) * 1.25)
    }
}

/// Canvas object kinds (R-CANVAS-003). Variants other than `Stroke` are placeholders until
/// their milestones; their commands return "not implemented".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObjectKind {
    Stroke(Stroke),
    /// A rich text box (R-TEXT-001): `content` with formatting `marks` over character
    /// ranges; laid out by cosmic-text at `size` canvas units per em, wrapped at `width`.
    Text {
        origin: Point,
        width: f64,
        content: String,
        #[serde(default)]
        marks: Vec<TextMark>,
        #[serde(default = "default_text_size")]
        size: f32,
    },
    /// A PDF (R-PDF-001): the listed pages of the document in `blob`, stacked as a vertical
    /// column from `origin` with `gap` between pages (canvas units); page sizes in PDF points
    /// times `scale`. A detached page is a PDF object with one page (R-PDF-002).
    Pdf {
        origin: Point,
        scale: f64,
        gap: f64,
        blob: String,
        pages: Vec<PdfPage>,
    },
    /// An image (R-CANVAS-003): the blob's picture shown in the rectangle `origin` +
    /// (`width`, `height`) canvas units; `pixels` is its decoded size.
    Image {
        origin: Point,
        width: f64,
        height: f64,
        blob: String,
        pixels: [u32; 2],
    },
    /// A link (R-LINK-001): a small label at `origin` that opens its target.
    Link {
        origin: Point,
        target: LinkTarget,
    },
    /// Audio (R-AUDIO-001): a recording (one Ogg Opus stream split at page boundaries into
    /// chunk blobs, in order) or an imported file (one blob, unchanged).
    Audio {
        origin: Point,
        blobs: Vec<String>,
        duration_ms: u64,
        #[serde(default)]
        recorded: bool,
    },
    /// A vector shape (R-SHAPE-001): `points` are the corners (polygon, rectangle, triangle,
    /// line/arrow ends) or, for an ellipse, two opposite corners of its bounding box.
    Shape {
        #[serde(default)]
        shape: ShapeKind,
        points: Vec<Point>,
        closed: bool,
        color: Color,
        width: f32,
    },
}

/// Prettified handwriting (ADR 0008 stage 1): display transforms of a block's strokes, affine
/// Size of an audio object's player on the canvas (canvas units).
pub const AUDIO_SIZE: (f64, f64) = (240.0, 48.0);

/// When a stroke was written during a recording (R-AUDIO-002): the recording (an audio
/// object's id) and the offset into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioMark {
    pub recording: ObjectId,
    pub offset_ms: u64,
}

/// `[a, b, c, d, e, f]` each. The strokes themselves never change; removing the parameters
/// shows them as written (R-HWR-005).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prettify {
    pub strokes: Vec<ObjectId>,
    pub transforms: Vec<[f64; 6]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    #[default]
    Polygon,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Triangle,
}

/// Where a link leads (stable ids: renaming or moving the target keeps it working, R-LINK-003).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum LinkTarget {
    /// A note, optionally at a stored view.
    Note {
        note: NoteId,
        view: Option<LinkView>,
    },
    Folder {
        folder: FolderId,
    },
    Url {
        url: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkView {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

/// Polylines that draw a shape (an arrow is its shaft and two head strokes; an ellipse is
/// approximated by 64 segments).
pub fn shape_outline(shape: ShapeKind, points: &[Point], closed: bool, width: f32) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    match shape {
        ShapeKind::Ellipse => {
            let (Some(a), Some(b)) = (points.first(), points.get(1)) else {
                return out;
            };
            let (cx, cy) = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
            let (rx, ry) = (((b.x - a.x) / 2.0).abs(), ((b.y - a.y) / 2.0).abs());
            out.push(
                (0..=64)
                    .map(|k| {
                        let t = f64::from(k) / 64.0 * std::f64::consts::TAU;
                        Point::new(cx + rx * t.cos(), cy + ry * t.sin())
                    })
                    .collect(),
            );
        }
        ShapeKind::Rectangle if points.len() == 2 => {
            let (Some(a), Some(b)) = (points.first(), points.get(1)) else {
                return out;
            };
            out.push(vec![*a, Point::new(b.x, a.y), *b, Point::new(a.x, b.y), *a]);
        }
        ShapeKind::Arrow => {
            let (Some(a), Some(b)) = (points.first(), points.last()) else {
                return out;
            };
            out.push(vec![*a, *b]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len = dx.hypot(dy);
            if len > 0.0 {
                let head = (len * 0.25).min(12.0 + 3.0 * f64::from(width));
                let (ux, uy) = (dx / len, dy / len);
                for s in [1.0f64, -1.0] {
                    let (c, n) = (0.5f64.cos(), s * 0.5f64.sin()); // ±28.6°
                    let (hx, hy) = (ux * c - uy * n, uy * c + ux * n);
                    out.push(vec![*b, Point::new(b.x - hx * head, b.y - hy * head)]);
                }
            }
        }
        _ => {
            let mut p: Vec<Point> = points.to_vec();
            if closed && let Some(first) = p.first().copied() {
                p.push(first);
            }
            out.push(p);
        }
    }
    out
}

/// Default text size (canvas units per em).
pub fn default_text_size() -> f32 {
    16.0
}

/// Formatting over the characters `start..end` of a text box. Paragraph styles (headings,
/// lists, checkboxes) apply to every line the range touches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextMark {
    pub start: u32,
    pub end: u32,
    #[serde(flatten)]
    pub style: TextStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum TextStyle {
    Bold,
    Italic,
    Underline,
    Strike,
    Heading1,
    Heading2,
    Heading3,
    BulletList,
    NumberedList,
    Checkbox {
        checked: bool,
    },
    Color {
        color: Color,
    },
    /// Font size in canvas units per em.
    Size {
        size: f32,
    },
}

impl TextStyle {
    /// Styles that apply to whole lines.
    pub fn is_paragraph(&self) -> bool {
        matches!(
            self,
            TextStyle::Heading1
                | TextStyle::Heading2
                | TextStyle::Heading3
                | TextStyle::BulletList
                | TextStyle::NumberedList
                | TextStyle::Checkbox { .. }
        )
    }
}

/// One page of a PDF object: its index in the document (0-based) and size in PDF points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PdfPage {
    pub index: u32,
    pub width: f64,
    pub height: f64,
}

/// Canvas rectangles of a PDF object's pages (top to bottom).
pub fn pdf_layout(origin: Point, scale: f64, gap: f64, pages: &[PdfPage]) -> Vec<Rect> {
    let mut y = origin.y;
    pages
        .iter()
        .map(|p| {
            let (w, h) = (p.width * scale, p.height * scale);
            let r = Rect {
                min: Point::new(origin.x, y),
                max: Point::new(origin.x + w, y + h),
            };
            y += h + gap;
            r
        })
        .collect()
}

impl ObjectKind {
    /// The blob a PDF or image shows.
    pub fn blob(&self) -> Option<&str> {
        match self {
            ObjectKind::Pdf { blob, .. } | ObjectKind::Image { blob, .. } => Some(blob),
            _ => None,
        }
    }

    /// Every blob the object refers to (blob garbage collection).
    pub fn blobs(&self) -> Vec<&str> {
        match self {
            ObjectKind::Pdf { blob, .. } | ObjectKind::Image { blob, .. } => vec![blob.as_str()],
            ObjectKind::Audio { blobs, .. } => blobs.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        }
    }

    /// Box of everything the object draws, or `None` if unknown (it then counts as covering
    /// the plane). Strokes: samples grown by 1.25 widths (a fully tilted highlighter).
    pub fn bounds(&self) -> Option<Rect> {
        let r = match self {
            ObjectKind::Stroke(s) => s.bounds(),
            ObjectKind::Pdf {
                origin,
                scale,
                gap,
                pages,
                ..
            } => {
                let mut r = Rect::EMPTY;
                for p in pdf_layout(*origin, *scale, *gap, pages) {
                    r.include(p.min);
                    r.include(p.max);
                }
                r
            }
            ObjectKind::Shape {
                shape,
                points,
                closed,
                width,
                ..
            } => {
                let mut r = Rect::EMPTY;
                for line in shape_outline(*shape, points, *closed, *width) {
                    for p in line {
                        r.include(p);
                    }
                }
                r.inflate(f64::from(*width))
            }
            ObjectKind::Image {
                origin, width, height, ..
            } => Rect {
                min: *origin,
                max: Point::new(origin.x + width, origin.y + height),
            },
            ObjectKind::Audio { origin, .. } => Rect {
                min: *origin,
                max: Point::new(origin.x + AUDIO_SIZE.0, origin.y + AUDIO_SIZE.1),
            },
            _ => return None,
        };
        (!r.is_empty()).then_some(r)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasObject {
    pub id: ObjectId,
    /// Z-order (higher draws on top).
    pub z: i64,
    #[serde(flatten)]
    pub kind: ObjectKind,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub objects: BTreeMap<ObjectId, CanvasObject>,
}
