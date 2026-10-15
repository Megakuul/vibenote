//! Search commands (R-SEARCH-001..003, R-PDF-004, R-TEXT-003).

use crate::command::{Registry, Spec, Typed};
use crate::search::Hit;
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryP {
    text: String,
    /// Most results (default 100, at most 500).
    limit: Option<usize>,
    /// Wait for indexing in progress (default true). `false` never blocks (the UI) and sets
    /// `indexing` when results may still be incomplete.
    wait: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InNoteP {
    text: String,
    /// Default: the current note.
    note: Option<u64>,
    /// As for `search.query`.
    wait: Option<bool>,
}

#[derive(Serialize)]
struct Group {
    note: u64,
    name: String,
    hits: Vec<Hit>,
}

#[derive(Serialize)]
struct Results {
    /// Every hit (folders first, then grouped by note).
    results: Vec<Hit>,
    /// The note hits grouped by note.
    groups: Vec<Group>,
    /// Indexing (handwriting recognition, PDF text) was still in progress (`wait: false`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    indexing: bool,
}

#[derive(Serialize)]
struct Regions {
    regions: Vec<Hit>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    indexing: bool,
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "search.query",
            doc: "Search names, text boxes, links and PDF text (case-, accent- and umlaut-insensitive); results grouped by note.",
            reqs: &["R-SEARCH-001", "R-SEARCH-002", "R-SEARCH-003", "R-PDF-004", "R-TEXT-003", "R-PERF-005"],
            implemented: true,
        },
        |s: &mut Session, p: QueryP| {
            let results = s.search(&p.text, p.limit.unwrap_or(100), None)?;
            let mut groups: Vec<Group> = Vec::new();
            for h in results.iter().filter(|h| h.note.is_some()) {
                let note = h.note.unwrap_or_default();
                match groups.last_mut() {
                    Some(g) if g.note == note => g.hits.push(h.clone()),
                    _ => groups.push(Group {
                        note,
                        name: s
                            .ws
                            .notes
                            .get(&vibenote_model::NoteId(note))
                            .map(|n| n.name.clone())
                            .unwrap_or_default(),
                        hits: vec![h.clone()],
                    }),
                }
            }
            let _ = p.wait; // used by `Engine::execute` before the command runs
            Ok(Results {
                results,
                groups,
                indexing: s.search_pending,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "search.in_note",
            doc: "Find in one note (default: the current one): the canvas regions of the matches.",
            reqs: &["R-SEARCH-002"],
            implemented: true,
        },
        |s: &mut Session, p: InNoteP| {
            let note = super::ink::target_note(s, p.note)?;
            let regions = s
                .search(&p.text, crate::search::MAX_RESULTS, Some(note))?
                .into_iter()
                .filter(|h| h.region.is_some())
                .collect();
            let _ = p.wait;
            Ok(Regions {
                regions,
                indexing: s.search_pending,
            })
        },
    )))?;
    Ok(())
}
