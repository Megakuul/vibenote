//! Z-order (R-CANVAS-005): bring objects to the front or send them to the back, keeping their
//! order among themselves. One undo step in the note's scope.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::Deserialize;
use vibenote_model::{CanvasObject, NoteId, ObjectId, Op};

use crate::MAX_IDS;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OrderP {
    note: Option<u64>,
    ids: Vec<u64>,
}

/// The objects `ids` of `note` in (z, id) order; completes a partially loaded note first if
/// any is not in memory (or always, with `complete`).
fn objects(s: &mut Session, note: NoteId, ids: &[u64], complete: bool) -> Result<Vec<CanvasObject>, EngineError> {
    if ids.len() > MAX_IDS {
        return Err(EngineError::Model(format!("at most {MAX_IDS} objects per command")));
    }
    let all_here = {
        let c = s.ws.canvas(note)?;
        ids.iter().all(|id| c.objects.contains_key(&ObjectId(*id)))
    };
    if complete || !all_here {
        s.ensure_complete(note)?;
    }
    let c = s.ws.canvas(note)?;
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut out = ids
        .into_iter()
        .map(|id| {
            c.objects
                .get(&ObjectId(id))
                .cloned()
                .ok_or_else(|| EngineError::Model(format!("object {id} not found")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|o| (o.z, o.id));
    Ok(out)
}

/// Re-numbers `objs` from `first` upwards (one op per object whose z changes).
fn renumber(note: NoteId, objs: Vec<CanvasObject>, first: i64) -> Result<Vec<Op>, EngineError> {
    let mut ops = Vec::with_capacity(objs.len());
    let mut z = first;
    for mut o in objs {
        if o.z != z {
            o.z = z;
            ops.push(Op::ReplaceObject { note, after: o });
        }
        z = z
            .checked_add(1)
            .ok_or_else(|| EngineError::Model("z-order range exhausted".into()))?;
    }
    Ok(ops)
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "object.raise",
            doc: "Bring objects to the front (above all others; their own order is kept). One undo step.",
            reqs: &["R-CANVAS-005"],
            implemented: true,
        },
        |s: &mut Session, p: OrderP| {
            let note = super::ink::target_note(s, p.note)?;
            let objs = objects(s, note, &p.ids, false)?;
            if objs.is_empty() {
                return Ok(());
            }
            let top = s.next_z(note)?;
            let ops = renumber(note, objs, top)?;
            s.commit(Scope::Note(note), "bring to front", ops)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "object.lower",
            doc: "Send objects to the back (below all others; their own order is kept). One undo step.",
            reqs: &["R-CANVAS-005"],
            implemented: true,
        },
        |s: &mut Session, p: OrderP| {
            let note = super::ink::target_note(s, p.note)?;
            // The bottom of a partially loaded note is not known without loading it.
            let objs = objects(s, note, &p.ids, true)?;
            let bottom = s.ws.canvas(note)?.objects.values().map(|o| o.z).min().unwrap_or(0);
            if objs.is_empty() {
                return Ok(());
            }
            let n = i64::try_from(objs.len()).map_err(|_| EngineError::Model("too many objects".into()))?;
            let first = bottom
                .checked_sub(n)
                .ok_or_else(|| EngineError::Model("z-order range exhausted".into()))?;
            let ops = renumber(note, objs, first)?;
            s.commit(Scope::Note(note), "send to back", ops)
        },
    )))?;
    Ok(())
}
