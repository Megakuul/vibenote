//! Folder and note management (R-ORG-001..004).

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::{Point, Rect};
use vibenote_model::{Canvas, Color, Folder, FolderId, Note, NoteId, Op};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FolderCreate {
    parent: Option<u64>,
    name: String,
    color: Option<u32>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FolderRename {
    id: u64,
    name: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FolderMove {
    id: u64,
    parent: Option<u64>,
    sort: Option<i64>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetColor {
    id: u64,
    color: Option<u32>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ById {
    id: u64,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteOpen {
    id: u64,
    /// Visible surface size in logical px (default 3840×2400); the objects in it load first.
    viewport: Option<[f64; 2]>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteCreate {
    folder: Option<u64>,
    name: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteMove {
    id: u64,
    folder: Option<u64>,
    sort: Option<i64>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NotesMove {
    ids: Vec<u64>,
    /// Target folder (null = root).
    folder: Option<u64>,
}

/// Most notes moved by one command (hostile input).
const MAX_MOVE: usize = 100_000;

#[derive(Serialize)]
struct Created {
    id: u64,
}

/// Default visible surface (logical px) when `note.open` gets none: a 4K screen at scale 1.
const DEFAULT_VIEWPORT: [f64; 2] = [3840.0, 2400.0];

/// Canvas area to load first: the visible surface plus a quarter on every side.
pub(crate) fn view_area(s: &Session, viewport: Option<[f64; 2]>) -> Result<Rect, EngineError> {
    let [w, h] = viewport.unwrap_or(DEFAULT_VIEWPORT);
    let ok = |v: f64| v.is_finite() && v > 0.0 && v <= 1.0e6;
    if !(ok(w) && ok(h)) {
        return Err(EngineError::Model(
            "viewport must be positive and at most 1e6 px".into(),
        ));
    }
    let v = s.view;
    let zoom = if v.zoom.is_finite() && v.zoom > 0.0 {
        v.zoom
    } else {
        1.0
    };
    let (cw, ch) = (w / zoom, h / zoom);
    Ok(Rect {
        min: Point::new(v.origin_x - cw / 4.0, v.origin_y - ch / 4.0),
        max: Point::new(v.origin_x + cw * 1.25, v.origin_y + ch * 1.25),
    })
}

/// Makes `id` the current note: its viewport loads first (R-PERF-004); it becomes the most
/// recent note (R-ORG-003).
pub(crate) fn open_note(s: &mut Session, id: u64, viewport: Option<[f64; 2]>) -> Result<(), EngineError> {
    note(s, id)?;
    let area = view_area(s, viewport)?;
    s.load_view(NoteId(id), &area)?;
    s.current_note = Some(NoteId(id));
    // Every cover is closed again (R-PEN-014).
    s.revealed.clear();
    s.recent.retain(|n| *n != NoteId(id));
    s.recent.insert(0, NoteId(id));
    s.recent.truncate(crate::quickopen::RECENT);
    s.meta_dirty.insert("recent");
    // Read the note's undo log now, not on the first pen-up (R-PERF-009).
    s.ensure_loaded(&crate::Scope::Note(NoteId(id)).key());
    // Its written blocks too (erasing checks them), and check them for re-recognition once
    // the note is complete.
    s.ensure_blocks(NoteId(id));
    s.sweep_notes.insert(NoteId(id));
    Ok(())
}

/// Next sort key at the end of a sibling list (saturating: never overflows).
fn next_sort<'a>(it: impl Iterator<Item = i64> + 'a) -> i64 {
    it.max().map_or(0, |m| m.saturating_add(1024))
}

fn folder(s: &Session, id: u64) -> Result<Folder, EngineError> {
    Ok(s.ws.folder(FolderId(id))?.clone())
}
fn note(s: &Session, id: u64) -> Result<Note, EngineError> {
    Ok(s.ws.note(NoteId(id))?.clone())
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.folder.create",
            doc: "Create a folder (optionally inside another).",
            reqs: &["R-ORG-001", "R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: FolderCreate| {
            let parent = p.parent.map(FolderId);
            let sort = next_sort(s.ws.folders.values().filter(|f| f.parent == parent).map(|f| f.sort));
            let id = FolderId(s.ws.alloc_id()?);
            let f = Folder {
                id,
                parent,
                name: p.name,
                color: p.color.map(Color),
                sort,
            };
            s.commit(Scope::Workspace, "create folder", vec![Op::CreateFolder { folder: f }])?;
            Ok(Created { id: id.0 })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.folder.rename",
            doc: "Rename a folder.",
            reqs: &["R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: FolderRename| {
            let after = Folder {
                name: p.name,
                ..folder(s, p.id)?
            };
            s.commit(Scope::Workspace, "rename folder", vec![Op::UpdateFolder { after }])
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.folder.move",
            doc: "Move/reorder a folder (parent null = root).",
            reqs: &["R-ORG-001", "R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: FolderMove| {
            let cur = folder(s, p.id)?;
            let parent = p.parent.map(FolderId);
            let sort = match p.sort {
                Some(v) => v,
                None => next_sort(
                    s.ws.folders
                        .values()
                        .filter(|f| f.parent == parent && f.id != cur.id)
                        .map(|f| f.sort),
                ),
            };
            s.commit(
                Scope::Workspace,
                "move folder",
                vec![Op::UpdateFolder {
                    after: Folder { parent, sort, ..cur },
                }],
            )
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.folder.set_color",
            doc: "Set or clear a folder colour (0xRRGGBBAA).",
            reqs: &["R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: SetColor| {
            let after = Folder {
                color: p.color.map(Color),
                ..folder(s, p.id)?
            };
            s.commit(Scope::Workspace, "colour folder", vec![Op::UpdateFolder { after }])
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.folder.delete",
            doc: "Move a folder to the trash; its notes and subfolders must already be in the trash (undoable).",
            reqs: &["R-ORG-002", "R-ORG-004"],
            implemented: true,
        },
        |s: &mut Session, p: ById| {
            let f = folder(s, p.id)?;
            s.commit(
                Scope::Workspace,
                "delete folder",
                vec![Op::TrashFolder { folder: f.id }],
            )
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.note.create",
            doc: "Create a note (empty infinite canvas).",
            reqs: &["R-ORG-002", "R-CANVAS-001"],
            implemented: true,
        },
        |s: &mut Session, p: NoteCreate| {
            let folder = p.folder.map(FolderId);
            let sort = next_sort(s.ws.notes.values().filter(|n| n.folder == folder).map(|n| n.sort));
            let id = NoteId(s.ws.alloc_id()?);
            let n = Note {
                id,
                folder,
                name: p.name,
                color: None,
                sort,
                background: None,
            };
            s.commit(
                Scope::Workspace,
                "create note",
                vec![Op::CreateNote {
                    note: n,
                    canvas: Canvas::default(),
                }],
            )?;
            Ok(Created { id: id.0 })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.note.rename",
            doc: "Rename a note.",
            reqs: &["R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: FolderRename| {
            let after = Note {
                name: p.name,
                ..note(s, p.id)?
            };
            s.commit(Scope::Workspace, "rename note", vec![Op::UpdateNote { after }])
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.note.move",
            doc: "Move/reorder a note (folder null = root).",
            reqs: &["R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: NoteMove| {
            let cur = note(s, p.id)?;
            let folder = p.folder.map(FolderId);
            let sort = match p.sort {
                Some(v) => v,
                None => next_sort(
                    s.ws.notes
                        .values()
                        .filter(|n| n.folder == folder && n.id != cur.id)
                        .map(|n| n.sort),
                ),
            };
            s.commit(
                Scope::Workspace,
                "move note",
                vec![Op::UpdateNote {
                    after: Note { folder, sort, ..cur },
                }],
            )
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.notes.move",
            doc: "Move several notes into a folder (null = root) at the end, keeping their order; one undo step.",
            reqs: &["R-ORG-003"],
            implemented: true,
        },
        |s: &mut Session, p: NotesMove| {
            if p.ids.len() > MAX_MOVE {
                return Err(EngineError::Model(format!("at most {MAX_MOVE} notes per move")));
            }
            let folder = p.folder.map(FolderId);
            if let Some(f) = folder {
                s.ws.folder(f)?;
            }
            let mut ids = p.ids;
            ids.sort_unstable();
            ids.dedup();
            let mut notes = ids.iter().map(|id| note(s, *id)).collect::<Result<Vec<_>, _>>()?;
            // Their current order: by folder, then sort key.
            notes.sort_by_key(|n| (n.folder, n.sort, n.id));
            let mut sort = next_sort(
                s.ws.notes
                    .values()
                    .filter(|n| n.folder == folder && ids.binary_search(&n.id.0).is_err())
                    .map(|n| n.sort),
            );
            let mut ops = Vec::with_capacity(notes.len());
            for n in notes {
                ops.push(Op::UpdateNote {
                    after: Note { folder, sort, ..n },
                });
                sort = sort.saturating_add(1024);
            }
            s.commit(Scope::Workspace, "move notes", ops)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.note.set_color",
            doc: "Set or clear a note colour (0xRRGGBBAA).",
            reqs: &["R-ORG-002"],
            implemented: true,
        },
        |s: &mut Session, p: SetColor| {
            let after = Note {
                color: p.color.map(Color),
                ..note(s, p.id)?
            };
            s.commit(Scope::Workspace, "colour note", vec![Op::UpdateNote { after }])
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "workspace.note.delete",
            doc: "Move a note to the trash (undoable; `trash.restore` brings it back, `trash.empty` deletes it).",
            reqs: &["R-ORG-002", "R-ORG-004"],
            implemented: true,
        },
        |s: &mut Session, p: ById| {
            let id = note(s, p.id)?.id;
            s.commit(Scope::Workspace, "delete note", vec![Op::TrashNote { note: id }])?;
            if s.current_note == Some(id) {
                s.current_note = None;
            }
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "note.open",
            doc: "Make a note the current note. Objects in the viewport load first, the rest of the canvas in the background.",
            reqs: &["R-ORG-003", "R-PERF-004"],
            implemented: true,
        },
        |s: &mut Session, p: NoteOpen| {
            super::text::end(s)?; // typing belongs to the note it happened in
            s.live.polygon.clear();
            open_note(s, p.id, p.viewport)
        },
    )))?;
    Ok(())
}
