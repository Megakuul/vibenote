//! Commands for UI surfaces (R-CMD-001: every user-visible action is a command). They only
//! record what the UI should show in `Session::ui`; programmatic calls never open dialogs.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    for (id, doc, reqs, panel) in [
        (
            "ui.quickopen.show",
            "Show quick-open (Ctrl+P).",
            &["R-ORG-003", "R-CMD-002"][..],
            "quickopen",
        ),
        (
            "ui.find.show",
            "Show find in the current note (Ctrl+F).",
            &["R-SEARCH-001", "R-CMD-002"][..],
            "find",
        ),
        (
            "ui.search.show",
            "Show search across the workspace (Ctrl+Shift+F).",
            &["R-SEARCH-002", "R-CMD-002"][..],
            "search",
        ),
    ] {
        r.register(Box::new(Typed::new(
            Spec {
                id,
                doc,
                reqs,
                implemented: true,
            },
            move |s: &mut Session, _: NoParams| {
                s.ui.panel = Some(panel);
                Ok(())
            },
        )))?;
    }
    r.register(Box::new(Typed::new(
        Spec {
            id: "ui.close",
            doc: "Close the open panel.",
            reqs: &["R-CMD-001"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            s.ui.panel = None;
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "app.quit",
            doc: "Ask the app to close (everything is saved first).",
            reqs: &["R-CMD-001", "R-CMD-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            s.ui.quit = true;
            Ok(())
        },
    )))?;
    Ok(())
}
