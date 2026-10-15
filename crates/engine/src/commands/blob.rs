//! Blob store commands (ADR 0005): read a blob's bytes (R-PDF-005: originals kept unmodified)
//! and collect unreferenced blobs.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Largest blob `blob.read` returns as JSON (bytes).
const MAX_READ: usize = 64 << 20;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadP {
    hash: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Serialize)]
struct Bytes {
    hash: String,
    len: usize,
    bytes: Vec<u8>,
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "blob.read",
            doc: "The bytes of a stored blob (checked against its hash; at most 64 MiB).",
            reqs: &["R-PDF-005", "R-DATA-001"],
            implemented: true,
        },
        |s: &mut Session, p: ReadP| {
            let b = s
                .blobs
                .0
                .get(&p.hash)
                .map_err(|e| EngineError::Storage(e.to_string()))?;
            if b.len() > MAX_READ {
                return Err(EngineError::Model(format!("blob larger than {MAX_READ} bytes")));
            }
            Ok(Bytes {
                hash: p.hash,
                len: b.len(),
                bytes: b.to_vec(),
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "blob.gc",
            doc: "Remove blobs that no object or undo step refers to (runs after storage is in sync).",
            reqs: &["R-DATA-001"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            s.gc_requested = true;
            Ok(())
        },
    )))?;
    Ok(())
}
