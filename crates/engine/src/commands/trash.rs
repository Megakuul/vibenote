//! Trash (R-ORG-004): deleting moves notes and folders to the trash (undoable, the canvas stays
//! on disk); `trash.restore` brings an item back together with its trashed parent folders;
//! `trash.empty` deletes everything in the trash permanently.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_model::{FolderId, NoteId, Op};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ById {
    /// A note or folder id.
    id: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Serialize)]
struct Item {
    id: u64,
    name: String,
    folder: Option<u64>,
}

#[derive(Serialize)]
struct Listing {
    notes: Vec<Item>,
    folders: Vec<Item>,
}

#[derive(Serialize)]
struct Emptied {
    notes: usize,
    folders: usize,
}

/// Trashed ancestors of `parent`, outermost first (they must be restored before their content).
fn trashed_ancestors(s: &Session, parent: Option<FolderId>) -> Vec<FolderId> {
    let mut chain = Vec::new();
    let mut cur = parent;
    for _ in 0..=vibenote_model::limits::MAX_FOLDER_DEPTH {
        let Some(f) = cur else { break };
        if s.ws.trashed_folders.contains(&f) {
            chain.push(f);
        }
        cur = s.ws.folders.get(&f).and_then(|x| x.parent);
    }
    chain.reverse();
    chain
}

fn depth(s: &Session, f: FolderId) -> usize {
    let mut d = 0usize;
    let mut cur = s.ws.folders.get(&f).and_then(|x| x.parent);
    while let Some(p) = cur {
        d = d.saturating_add(1);
        if d > vibenote_model::limits::MAX_FOLDER_DEPTH {
            break;
        }
        cur = s.ws.folders.get(&p).and_then(|x| x.parent);
    }
    d
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "trash.restore",
            doc: "Restore a note or folder from the trash (with its trashed parent folders; undoable).",
            reqs: &["R-ORG-004"],
            implemented: true,
        },
        |s: &mut Session, p: ById| {
            let mut ops: Vec<Op> = Vec::new();
            if let Some(n) = s.ws.notes.get(&NoteId(p.id)) {
                ops.extend(
                    trashed_ancestors(s, n.folder)
                        .into_iter()
                        .map(|folder| Op::RestoreFolder { folder }),
                );
                if s.ws.trashed_notes.contains(&n.id) {
                    ops.push(Op::RestoreNote { note: n.id });
                }
            } else if let Some(f) = s.ws.folders.get(&FolderId(p.id)) {
                ops.extend(
                    trashed_ancestors(s, Some(f.id))
                        .into_iter()
                        .map(|folder| Op::RestoreFolder { folder }),
                );
            } else {
                return Err(EngineError::Model(format!("no note or folder {}", p.id)));
            }
            if ops.is_empty() {
                return Err(EngineError::Model(format!("{} is not in the trash", p.id)));
            }
            s.commit(Scope::Workspace, "restore", ops)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "trash.list",
            doc: "Notes and folders in the trash.",
            reqs: &["R-ORG-004"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            let notes =
                s.ws.trashed_notes
                    .iter()
                    .filter_map(|id| s.ws.notes.get(id))
                    .map(|n| Item {
                        id: n.id.0,
                        name: n.name.clone(),
                        folder: n.folder.map(|f| f.0),
                    })
                    .collect();
            let folders =
                s.ws.trashed_folders
                    .iter()
                    .filter_map(|id| s.ws.folders.get(id))
                    .map(|f| Item {
                        id: f.id.0,
                        name: f.name.clone(),
                        folder: f.parent.map(|p| p.0),
                    })
                    .collect();
            Ok(Listing { notes, folders })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "trash.empty",
            doc: "Permanently delete everything in the trash (not undoable; clears the workspace undo history).",
            reqs: &["R-ORG-004"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            let notes: Vec<NoteId> = s.ws.trashed_notes.iter().copied().collect();
            let mut folders: Vec<FolderId> = s.ws.trashed_folders.iter().copied().collect();
            // Innermost folders first: each is empty once its contents are gone.
            folders.sort_by_key(|f| std::cmp::Reverse(depth(s, *f)));
            let mut ops = Vec::with_capacity(notes.len().saturating_add(folders.len()));
            for id in &notes {
                ops.push(Op::DeleteNote {
                    note: s.ws.note(*id)?.clone(),
                });
            }
            for id in &folders {
                ops.push(Op::DeleteFolder {
                    folder: s.ws.folder(*id)?.clone(),
                });
            }
            s.apply_permanent(ops, &notes)?;
            s.gc_requested = true; // their PDFs, images and audio go too
            Ok(Emptied {
                notes: notes.len(),
                folders: folders.len(),
            })
        },
    )))?;
    Ok(())
}
