//! Undo / redo (R-UNDO-001).

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UndoP {
    /// "workspace" for folder/note management; a note id for that note's history; default: the current note.
    scope: Option<ScopeP>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum ScopeP {
    Note(u64),
    Named(String),
}

#[derive(Serialize)]
struct Done {
    label: String,
}

fn scope(s: &Session, p: Option<ScopeP>) -> Result<Scope, EngineError> {
    match p {
        Some(ScopeP::Named(n)) if n == "workspace" => Ok(Scope::Workspace),
        Some(ScopeP::Named(n)) => Err(EngineError::Model(format!("unknown scope `{n}`"))),
        Some(ScopeP::Note(id)) => Ok(Scope::Note(super::ink::target_note(s, Some(id))?)),
        None => s
            .current_note
            .map(Scope::Note)
            .ok_or_else(|| EngineError::Model("no scope and no current note".into())),
    }
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "edit.undo",
            doc: "Undo the last step in a scope.",
            reqs: &["R-UNDO-001"],
            implemented: true,
        },
        |s: &mut Session, p: UndoP| {
            let sc = scope(s, p.scope)?;
            Ok(Done { label: s.undo(sc)? })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "edit.redo",
            doc: "Redo the last undone step in a scope.",
            reqs: &["R-UNDO-001"],
            implemented: true,
        },
        |s: &mut Session, p: UndoP| {
            let sc = scope(s, p.scope)?;
            Ok(Done { label: s.redo(sc)? })
        },
    )))?;
    Ok(())
}
