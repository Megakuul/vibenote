//! Registered but not yet implemented commands. Each maps to SPEC requirement ids so the
//! parity catalog lists them; their tests start red (AGENTS.md §2 Phase 3).

use crate::EngineError;
use crate::command::{Registry, Spec, Stub};

const STUBS: &[(&str, &str, &[&str])] = &[];

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    for (id, doc, reqs) in STUBS {
        r.register(Box::new(Stub(Spec {
            id,
            doc,
            reqs,
            implemented: false,
        })))?;
    }
    Ok(())
}
