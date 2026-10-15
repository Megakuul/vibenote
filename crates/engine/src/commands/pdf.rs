//! PDF commands (R-PDF-001/005, R-PERF-006, R-CANVAS-004): import a PDF as one object whose
//! pages form a vertical column, render a page, and hit-test the canvas.

use crate::command::{Registry, Spec, Typed};
use crate::pdf::PageKey;
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use vibenote_geom::Point;
use vibenote_model::{CanvasObject, NoteId, ObjectId, ObjectKind, Op, PdfPage, pdf_layout};

/// Canvas units per PDF point (96 dpi: a page at 100 % zoom is about its printed size).
pub const PDF_SCALE: f64 = 96.0 / 72.0;
/// Gap between the pages of a column (canvas units).
pub const PDF_GAP: f64 = 24.0;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportP {
    /// File to import (read once; the bytes are kept unmodified in the blob store).
    path: String,
    /// Canvas point of the first page's top-left corner.
    at: [f64; 2],
    note: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct PageOut {
    /// 1-based page number in the document.
    page: u32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Serialize)]
struct Imported {
    id: u64,
    blob: String,
    gap: f64,
    pages: Vec<PageOut>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenderP {
    /// The PDF object.
    id: u64,
    /// 1-based position of the page within the object.
    page: u32,
    /// Bitmap width in pixels (rounded up to the next cached size, 256–4096).
    width_px: u32,
    note: Option<u64>,
}

#[derive(Serialize)]
struct Rendered {
    width: u32,
    height: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DetachP {
    /// The PDF object.
    id: u64,
    /// 1-based position of the page within the object.
    page: u32,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RejoinP {
    /// The object whose pages go back.
    id: u64,
    /// The PDF object (same document) that takes them.
    into: u64,
    note: Option<u64>,
}

#[derive(Serialize)]
struct Detached {
    /// The new single-page object.
    id: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HitP {
    /// Canvas point.
    at: [f64; 2],
    /// Tolerance in canvas units (default: 4 screen pixels at the current zoom).
    radius: Option<f64>,
    note: Option<u64>,
}

#[derive(Serialize)]
struct Hits {
    /// Topmost first.
    ids: Vec<u64>,
}

pub(crate) fn pages_out(origin: Point, scale: f64, gap: f64, pages: &[PdfPage]) -> Vec<PageOut> {
    pdf_layout(origin, scale, gap, pages)
        .into_iter()
        .zip(pages)
        .map(|(r, p)| PageOut {
            page: p.index.saturating_add(1),
            x: r.min.x,
            y: r.min.y,
            width: r.max.x - r.min.x,
            height: r.max.y - r.min.y,
        })
        .collect()
}

fn read_file(path: &str) -> Result<Arc<[u8]>, EngineError> {
    let p = std::path::Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| EngineError::Model(format!("{path}: {e}")))?;
    if !meta.is_file() {
        return Err(EngineError::Model(format!("{path}: not a file")));
    }
    if meta.len() > vibenote_pdf::proto::MAX_PDF_BYTES {
        return Err(EngineError::Model(format!("{path}: larger than 1 GiB")));
    }
    let bytes = std::fs::read(p).map_err(|e| EngineError::Model(format!("{path}: {e}")))?;
    Ok(Arc::from(bytes))
}

/// The document number of a PDF object's blob (registered with the service on first use).
/// The document number of a PDF blob; its bytes are read later, when the worker opens it.
pub(crate) fn doc_of(s: &Session, blob: &str) -> u64 {
    let blobs = Arc::clone(&s.blobs.0);
    let key = blob.to_string();
    s.pdf.doc(
        blob,
        Arc::new(move || blobs.get(&key).map_err(|e| EngineError::Storage(e.to_string()))),
    )
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "pdf.import",
            doc: "Import a PDF as one object: its pages in a vertical column with a fixed gap; the original bytes are kept unmodified (one undo step).",
            reqs: &["R-PDF-001", "R-PDF-003", "R-PDF-005", "R-DATA-004"],
            implemented: true,
        },
        |s: &mut Session, p: ImportP| {
            let note = super::ink::target_note(s, p.note)?;
            let [x, y] = p.at;
            let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
            if !(ok(x) && ok(y)) {
                return Err(EngineError::Model("`at` out of range".into()));
            }
            let bytes = read_file(&p.path)?;
            let blob = vibenote_storage::blobs::hash(&bytes);
            // The worker checks the file before anything is stored.
            let sizes = s.pdf.open(&blob, Arc::clone(&bytes))?;
            let pages: Vec<PdfPage> = sizes
                .iter()
                .zip(0u32..)
                .map(|(sz, index)| PdfPage {
                    index,
                    width: sz.width,
                    height: sz.height,
                })
                .collect();
            // Durable before the object that refers to it (ADR 0005).
            s.blobs.0.put(&bytes).map_err(|e| EngineError::Storage(e.to_string()))?;
            let origin = Point::new(x, y);
            let z = s.next_z(note)?;
            let id = ObjectId(s.ws.alloc_id()?);
            let object = CanvasObject {
                id,
                z,
                kind: ObjectKind::Pdf {
                    origin,
                    scale: PDF_SCALE,
                    gap: PDF_GAP,
                    blob: blob.clone(),
                    pages: pages.clone(),
                },
            };
            s.commit(Scope::Note(note), "import PDF", vec![Op::InsertObject { note, object }])?;
            let doc = doc_of(s, &blob);
            // Text for search, in the background (R-PDF-004).
            s.pdf.want_text(doc);
            Ok(Imported {
                id: id.0,
                blob,
                gap: PDF_GAP,
                pages: pages_out(origin, PDF_SCALE, PDF_GAP, &pages),
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "pdf.render_page",
            doc: "Render one page of a PDF object now (cached); returns the bitmap size.",
            reqs: &["R-PDF-003", "R-PERF-006"],
            implemented: true,
        },
        |s: &mut Session, p: RenderP| {
            let note = super::ink::target_note(s, p.note)?;
            let o = object(s, note, p.id)?;
            let ObjectKind::Pdf { blob, pages, .. } = &o.kind else {
                return Err(EngineError::Model(format!("object {} is not a PDF", p.id)));
            };
            let page = pages
                .get(usize::try_from(p.page).unwrap_or(usize::MAX).wrapping_sub(1))
                .ok_or_else(|| EngineError::Model(format!("no page {} in object {}", p.page, p.id)))?;
            let doc = doc_of(s, blob);
            let b = s.pdf.render_now(PageKey {
                doc,
                page: page.index,
                width: crate::pdf::width_for(f64::from(p.width_px)),
            })?;
            Ok(Rendered {
                width: b.width,
                height: b.height,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "pdf.page_detach",
            doc: "Split a page out of a PDF object into its own object where it was; the rest of the column closes up (one undo step).",
            reqs: &["R-PDF-002"],
            implemented: true,
        },
        |s: &mut Session, p: DetachP| {
            let note = super::ink::target_note(s, p.note)?;
            let mut o = object(s, note, p.id)?;
            let ObjectKind::Pdf {
                origin,
                scale,
                gap,
                blob,
                pages,
            } = &mut o.kind
            else {
                return Err(EngineError::Model(format!("object {} is not a PDF", p.id)));
            };
            if pages.len() < 2 {
                return Err(EngineError::Model("a single page cannot be detached".into()));
            }
            let k = usize::try_from(p.page).unwrap_or(usize::MAX).wrapping_sub(1);
            let rect = *pdf_layout(*origin, *scale, *gap, pages)
                .get(k)
                .ok_or_else(|| EngineError::Model(format!("no page {} in object {}", p.page, p.id)))?;
            let page = pages.remove(k);
            let detached = ObjectKind::Pdf {
                origin: rect.min,
                scale: *scale,
                gap: *gap,
                blob: blob.clone(),
                pages: vec![page],
            };
            let z = s.next_z(note)?;
            let id = ObjectId(s.ws.alloc_id()?);
            s.commit(
                Scope::Note(note),
                "detach page",
                vec![
                    Op::ReplaceObject { note, after: o },
                    Op::InsertObject {
                        note,
                        object: CanvasObject { id, z, kind: detached },
                    },
                ],
            )?;
            Ok(Detached { id: id.0 })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "pdf.page_rejoin",
            doc: "Put the pages of a detached PDF object back into another object of the same document, in document order (one undo step).",
            reqs: &["R-PDF-002"],
            implemented: true,
        },
        |s: &mut Session, p: RejoinP| {
            let note = super::ink::target_note(s, p.note)?;
            if p.id == p.into {
                return Err(EngineError::Model("cannot rejoin an object into itself".into()));
            }
            let from = object(s, note, p.id)?;
            let mut into = object(s, note, p.into)?;
            let (ObjectKind::Pdf { blob: b1, pages: moved, .. }, ObjectKind::Pdf { blob: b2, pages, .. }) =
                (&from.kind, &mut into.kind)
            else {
                return Err(EngineError::Model("both objects must be PDFs".into()));
            };
            if b1 != b2 {
                return Err(EngineError::Model("the objects show different documents".into()));
            }
            if moved.iter().any(|m| pages.iter().any(|q| q.index == m.index)) {
                return Err(EngineError::Model("a page would appear twice".into()));
            }
            pages.extend(moved.iter().copied());
            pages.sort_by_key(|q| q.index);
            s.commit(
                Scope::Note(note),
                "rejoin pages",
                vec![
                    Op::RemoveObject { note, object: from },
                    Op::ReplaceObject { note, after: into },
                ],
            )?;
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "canvas.hit_test",
            doc: "Objects under a canvas point, topmost first (strokes by their ink, other objects by their area).",
            reqs: &["R-CANVAS-004", "R-PEN-009"],
            implemented: true,
        },
        |s: &mut Session, p: HitP| {
            let note = super::ink::target_note(s, p.note)?;
            let radius = p.radius.unwrap_or(4.0 / s.zoom());
            Ok(Hits {
                ids: hits_at(s, note, (p.at[0], p.at[1]), radius)?,
            })
        },
    )))?;
    Ok(())
}

/// Objects of `note` under canvas point `at`, topmost first: strokes by their ink within
/// `radius`, other objects by their box (text boxes and links by their layout estimate).
pub(crate) fn hits_at(s: &mut Session, note: NoteId, at: (f64, f64), radius: f64) -> Result<Vec<u64>, EngineError> {
    let e = vibenote_ink::erase::Eraser { path: &[at], radius };
    e.check().map_err(EngineError::Model)?;
    let mut ids = s.candidates_along(note, &[at], radius)?;
    // Text boxes and links have no stored box (their layout decides it): test them all.
    let c = s.ws.canvas(note)?;
    ids.extend(s.non_stroke_ids(note, c).into_iter().filter(|id| {
        matches!(
            c.objects.get(id).map(|o| &o.kind),
            Some(ObjectKind::Text { .. } | ObjectKind::Link { .. })
        )
    }));
    ids.sort_unstable();
    ids.dedup();
    let mut hits: Vec<(i64, ObjectId)> = ids
        .into_iter()
        .filter_map(|id| {
            let o = c.objects.get(&id)?;
            let hit = match &o.kind {
                ObjectKind::Stroke(st) => vibenote_ink::erase::touches(st, &e),
                ObjectKind::Pdf {
                    origin,
                    scale,
                    gap,
                    pages,
                    ..
                } => pdf_layout(*origin, *scale, *gap, pages).iter().any(|r| {
                    r.min.x - radius <= at.0
                        && at.0 <= r.max.x + radius
                        && r.min.y - radius <= at.1
                        && at.1 <= r.max.y + radius
                }),
                _ => s
                    .object_box(o)
                    .is_some_and(|b| b.inflate(radius).contains(Point::new(at.0, at.1))),
            };
            hit.then_some((o.z, o.id))
        })
        .collect();
    hits.sort_by(|a, b| b.cmp(a));
    Ok(hits.into_iter().map(|(_, id)| id.0).collect())
}

/// An object of `note`, loading the note completely if it is not in memory yet.
pub(crate) fn object(s: &mut Session, note: NoteId, id: u64) -> Result<CanvasObject, EngineError> {
    if !s.ws.canvas(note)?.objects.contains_key(&ObjectId(id)) {
        s.ensure_complete(note)?;
    }
    s.ws.canvas(note)?
        .objects
        .get(&ObjectId(id))
        .cloned()
        .ok_or_else(|| EngineError::Model(format!("object {id} not found")))
}
