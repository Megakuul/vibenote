//! The finder (R-VIM-005): a telescope-like, keyboard-only picker. `finder.query` ranks lines
//! of typed text and recognised handwriting (kind "text", Space f w) or notes by name (kind
//! "notes", Space f f / Ctrl+P) with fuzzy matching; `finder.open` jumps to a result.

use super::quickopen::path;
use crate::command::{Registry, Spec, Typed};
use crate::quickopen::{MAX_LIMIT, MAX_QUERY, fold, score, trashed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::{Point, Rect};
use vibenote_model::{NoteId, ObjectId, ObjectKind};

/// Most indexed rows (text boxes, handwriting blocks) the text finder looks through.
const MAX_ROWS: usize = 20_000;
/// Lines of context in a text result's preview, before and after the line.
const PREVIEW_CONTEXT: usize = 4;
/// Most lines in a preview.
const PREVIEW_LINES: usize = 12;

#[derive(Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FinderKind {
    Text,
    Notes,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryP {
    kind: FinderKind,
    #[serde(default)]
    query: String,
    /// Most results (default 50, at most 200).
    limit: Option<usize>,
}

#[derive(Serialize, Clone)]
struct Found {
    note: u64,
    /// The note's name and folder path ("Uni/Analysis").
    name: String,
    path: String,
    /// Text results: the matching line, the object (text box or handwriting block), the line's
    /// first character in a text box, the object's region.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<[f64; 4]>,
    /// A PDF page's text: its page number (1-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    /// Lines around the match (text) or the note's first lines (notes).
    preview: Vec<String>,
    score: i64,
}

#[derive(Serialize)]
struct Results {
    results: Vec<Found>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenP {
    note: u64,
    /// A text box or handwriting block to centre (and a text box to type into at `offset`).
    object: Option<u64>,
    offset: Option<u32>,
    /// The area to centre when the object is not a loaded text box: [x0, y0, x1, y1].
    region: Option<[f64; 4]>,
    /// Visible surface size in logical px.
    viewport: Option<[f64; 2]>,
}

fn rect4(r: Rect) -> [f64; 4] {
    [r.min.x, r.min.y, r.max.x, r.max.y]
}

fn text_query(s: &Session, query: &str, limit: usize) -> Result<Vec<Found>, EngineError> {
    let q: Vec<char> = fold(query).into_iter().take(MAX_QUERY).collect();
    let rows = s.written_rows(MAX_ROWS)?;
    let ws = &s.ws;
    let mut found: Vec<Found> = Vec::new();
    for r in rows {
        let Some(n) = ws.notes.get(&r.note) else { continue };
        if trashed(ws, n) {
            continue;
        }
        let lines: Vec<&str> = r.raw.split('\n').collect();
        let mut offset = 0u32;
        for (li, line) in lines.iter().enumerate() {
            let start = offset;
            offset = offset
                .saturating_add(u32::try_from(line.chars().count()).unwrap_or(u32::MAX))
                .saturating_add(1);
            if line.trim().is_empty() {
                continue;
            }
            let Some(sc) = score(&q, &fold(line)) else { continue };
            let from = li.saturating_sub(PREVIEW_CONTEXT);
            let preview = lines
                .iter()
                .skip(from)
                .take(PREVIEW_CONTEXT.saturating_mul(2).saturating_add(1).min(PREVIEW_LINES))
                .map(|l| (*l).to_string())
                .collect();
            found.push(Found {
                note: r.note.0,
                name: n.name.clone(),
                path: path(ws, n.folder),
                line: Some((*line).to_string()),
                object: Some(r.object.0),
                offset: (r.source == "text").then_some(start),
                region: r.region.map(rect4),
                page: if r.source == "pdf" { r.page } else { None },
                preview,
                score: sc,
            });
        }
    }
    // Best first; equal scores keep the notes' order (stable sort).
    found.sort_by_key(|f| std::cmp::Reverse(f.score));
    found.truncate(limit);
    Ok(found)
}

fn notes_query(s: &mut Session, query: &str, limit: usize) -> Result<Vec<Found>, EngineError> {
    let ws = &s.ws;
    let live = |id: &NoteId| ws.notes.get(id).is_some_and(|n| !trashed(ws, n));
    let recent: Vec<NoteId> = s.recent.iter().copied().filter(live).collect();
    let mut found: Vec<(NoteId, i64)> = if query.trim().is_empty() {
        // Recent notes first, then the rest by name.
        let mut all: Vec<(NoteId, i64)> = recent.iter().map(|id| (*id, 0)).collect();
        let mut rest: Vec<&vibenote_model::Note> = ws
            .notes
            .values()
            .filter(|n| !trashed(ws, n) && !recent.contains(&n.id))
            .collect();
        rest.sort_by_key(|n| n.name.to_lowercase());
        all.extend(rest.into_iter().map(|n| (n.id, 0)));
        all
    } else {
        // Name and folder path together ("uni lin" finds Uni/Lineare Algebra).
        let q: Vec<char> = fold(query).into_iter().take(MAX_QUERY).collect();
        let mut v: Vec<(NoteId, i64)> = ws
            .notes
            .values()
            .filter(|n| !trashed(ws, n))
            .filter_map(|n| {
                let by_name = score(&q, &fold(&n.name));
                let p = path(ws, n.folder);
                let by_path = (!p.is_empty())
                    .then(|| score(&q, &fold(&format!("{p}/{}", n.name))).map(|x| x.saturating_sub(50)))
                    .flatten();
                by_name.max(by_path).map(|x| (n.id, x))
            })
            .collect();
        let rank = |id: NoteId| recent.iter().position(|r| *r == id).unwrap_or(usize::MAX);
        v.sort_by_key(|&(id, x)| (std::cmp::Reverse(x), rank(id), id));
        v
    };
    found.truncate(limit);
    // Previews: the first lines written in each note.
    let rows = if found.is_empty() {
        Vec::new()
    } else {
        s.written_rows(MAX_ROWS)?
    };
    Ok(found
        .into_iter()
        .filter_map(|(id, sc)| {
            let n = s.ws.notes.get(&id)?;
            let preview = rows
                .iter()
                .filter(|r| r.note == id)
                .flat_map(|r| r.raw.lines())
                .filter(|l| !l.trim().is_empty())
                .take(PREVIEW_LINES)
                .map(str::to_string)
                .collect();
            Some(Found {
                note: id.0,
                name: n.name.clone(),
                path: path(&s.ws, n.folder),
                line: None,
                object: None,
                offset: None,
                region: None,
                page: None,
                preview,
                score: sc,
            })
        })
        .collect())
}

/// Opens `note`, centres the object (or region), and starts typing into a text box at
/// `offset`.
fn open(s: &mut Session, p: &OpenP) -> Result<(), EngineError> {
    super::text::end(s)?;
    s.live.polygon.clear();
    super::workspace::open_note(s, p.note, p.viewport)?;
    let note = NoteId(p.note);
    let obj_box = |s: &Session| -> Option<(Rect, bool)> {
        let o = s.ws.canvas(note).ok()?.objects.get(&ObjectId(p.object?))?;
        Some((s.object_box(o)?, matches!(o.kind, ObjectKind::Text { .. })))
    };
    let target = obj_box(s).map(|(b, _)| b).or_else(|| {
        p.region.map(|[x0, y0, x1, y1]| Rect {
            min: Point::new(x0, y0),
            max: Point::new(x1, y1),
        })
    });
    if let Some(b) = target.filter(|b| b.min.x.is_finite() && b.max.y.is_finite()) {
        let [w, h] = p.viewport.unwrap_or([1280.0, 800.0]);
        let v = s.view;
        let zoom = if v.zoom.is_finite() && v.zoom > 0.0 {
            v.zoom
        } else {
            1.0
        };
        let (cx, cy) = ((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0);
        s.view = crate::View {
            origin_x: cx - w / 2.0 / zoom,
            origin_y: cy - h / 2.0 / zoom,
            zoom,
        };
        let area = super::workspace::view_area(s, p.viewport)?;
        s.load_view(note, &area)?;
    }
    if let (Some(id), Some((_, true))) = (p.object, obj_box(s)) {
        super::text::open_at(s, note, id, p.offset.unwrap_or(0))?;
    }
    Ok(())
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "finder.query",
            doc: "The finder's results, best first: kind \"text\" ranks lines of typed text and recognised handwriting in all notes by fuzzy match (Space f w); kind \"notes\" ranks notes by name and folder path (Space f f, Ctrl+P; empty: recent notes, then all). Each result has a preview.",
            reqs: &["R-VIM-005"],
            implemented: true,
        },
        |s: &mut Session, p: QueryP| {
            let limit = p.limit.unwrap_or(50).min(MAX_LIMIT);
            let results = match p.kind {
                FinderKind::Text => text_query(s, &p.query, limit)?,
                FinderKind::Notes => notes_query(s, &p.query, limit)?,
            };
            Ok(Results { results })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "finder.open",
            doc: "Open a finder result: the note, centred on the object or region; a text box is put into editing at `offset` (Normal mode with vim on).",
            reqs: &["R-VIM-005"],
            implemented: true,
        },
        |s: &mut Session, p: OpenP| open(s, &p),
    )))?;
    Ok(())
}
