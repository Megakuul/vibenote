//! Quick-open (R-ORG-003, R-PERF-008): fuzzy search of notes by name; with no text, the
//! recently opened notes.

use crate::command::{Registry, Spec, Typed};
use crate::quickopen::{MAX_LIMIT, trashed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_model::{FolderId, NoteId, Workspace};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Query {
    /// Typed text; empty = recent notes.
    #[serde(default)]
    text: String,
    /// Most results (default 20, at most 200).
    limit: Option<usize>,
}

#[derive(Serialize)]
struct Hit {
    id: u64,
    name: String,
    /// Folder names from the root, e.g. "Uni/Analysis".
    path: String,
    score: i64,
    recent: bool,
}

#[derive(Serialize)]
struct Results {
    results: Vec<Hit>,
}

/// "A/B/C" for a folder chain (bounded depth; cycles cannot occur in a valid workspace).
fn path(ws: &Workspace, folder: Option<FolderId>) -> String {
    let mut parts = Vec::new();
    let mut cur = folder;
    for _ in 0..=vibenote_model::limits::MAX_FOLDER_DEPTH {
        let Some(f) = cur.and_then(|id| ws.folders.get(&id)) else {
            break;
        };
        parts.push(f.name.as_str());
        cur = f.parent;
    }
    parts.reverse();
    parts.join("/")
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "quickopen.query",
            doc: "Fuzzy-find notes by name (best first; recent notes win ties); empty text lists recent notes.",
            reqs: &["R-ORG-003", "R-PERF-008"],
            implemented: true,
        },
        |s: &mut Session, p: Query| {
            let limit = p.limit.unwrap_or(20).min(MAX_LIMIT);
            let ws = &s.ws;
            let live = |id: &NoteId| ws.notes.get(id).is_some_and(|n| !trashed(ws, n));
            let recent: Vec<NoteId> = s.recent.iter().copied().filter(live).collect();
            let found: Vec<(NoteId, i64)> = if p.text.trim().is_empty() {
                recent.iter().take(limit).map(|id| (*id, 0)).collect()
            } else {
                s.quick.query(&s.ws, s.notes_rev, &p.text, limit, &recent)
            };
            let results = found
                .into_iter()
                .filter_map(|(id, score)| {
                    let n = s.ws.notes.get(&id)?;
                    Some(Hit {
                        id: id.0,
                        name: n.name.clone(),
                        path: path(&s.ws, n.folder),
                        score,
                        recent: recent.contains(&id),
                    })
                })
                .collect();
            Ok(Results { results })
        },
    )))?;
    Ok(())
}
