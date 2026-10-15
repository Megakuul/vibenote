//! Image import (R-CANVAS-003, R-DATA-004): PNG, JPEG, WebP and SVG files become image objects;
//! the original bytes are kept unmodified in the blob store.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::Point;
use vibenote_model::{CanvasObject, ObjectId, ObjectKind, Op};

/// Default display width cap (canvas units): larger images are shown scaled down.
const DEFAULT_MAX_WIDTH: f64 = 800.0;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportP {
    path: String,
    /// Canvas point of the top-left corner.
    at: [f64; 2],
    /// Display width in canvas units (default: its pixel width, at most 800).
    width: Option<f64>,
    note: Option<u64>,
}

#[derive(Serialize)]
struct Imported {
    id: u64,
    blob: String,
    pixels: [u32; 2],
    width: f64,
    height: f64,
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "image.import",
            doc: "Import a PNG, JPEG, WebP or SVG image as an object (original bytes kept; one undo step).",
            reqs: &["R-CANVAS-003", "R-DATA-004"],
            implemented: true,
        },
        |s: &mut Session, p: ImportP| {
            let note = super::ink::target_note(s, p.note)?;
            let [x, y] = p.at;
            let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
            if !(ok(x) && ok(y)) {
                return Err(EngineError::Model("`at` out of range".into()));
            }
            let path = std::path::Path::new(&p.path);
            let meta = std::fs::metadata(path).map_err(|e| EngineError::Model(format!("{}: {e}", p.path)))?;
            if !meta.is_file() || meta.len() > crate::images::MAX_FILE {
                return Err(EngineError::Model(format!(
                    "{}: not a file or larger than 256 MiB",
                    p.path
                )));
            }
            let bytes = std::fs::read(path).map_err(|e| EngineError::Model(format!("{}: {e}", p.path)))?;
            let (pw, ph) = crate::images::probe(&bytes)?;
            let width = match p.width {
                Some(w) if w.is_finite() && w > 0.0 && w <= 1.0e7 => w,
                Some(_) => return Err(EngineError::Model("width out of range".into())),
                None => f64::from(pw).min(DEFAULT_MAX_WIDTH),
            };
            let height = width * f64::from(ph) / f64::from(pw);
            let blob = s.blobs.0.put(&bytes).map_err(|e| EngineError::Storage(e.to_string()))?;
            let z = s.next_z(note)?;
            let id = ObjectId(s.ws.alloc_id()?);
            let object = CanvasObject {
                id,
                z,
                kind: ObjectKind::Image {
                    origin: Point::new(x, y),
                    width,
                    height,
                    blob: blob.clone(),
                    pixels: [pw, ph],
                },
            };
            s.commit(
                Scope::Note(note),
                "import image",
                vec![Op::InsertObject { note, object }],
            )?;
            Ok(Imported {
                id: id.0,
                blob,
                pixels: [pw, ph],
                width,
                height,
            })
        },
    )))?;
    Ok(())
}
