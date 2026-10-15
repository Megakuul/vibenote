#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Change feed (M3.1b): replaying each note's journal onto a mirror always reproduces the
//! canvas, whatever commands, undo/redo, trash or background loading did.

use proptest::prelude::*;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use vibenote_engine::Engine;
use vibenote_model::{CanvasObject, NoteId, ObjectId, Since};

#[derive(Default)]
struct Mirror {
    notes: HashMap<NoteId, (u64, u64, BTreeMap<ObjectId, CanvasObject>)>,
}

impl Mirror {
    /// Catches up like a renderer would and checks the result.
    fn check(&mut self, e: &Engine) -> Result<(), TestCaseError> {
        for (id, canvas) in &e.session.ws.canvases {
            let Some(log) = e.session.change_log(*id) else {
                prop_assert!(canvas.objects.is_empty(), "note {id} has objects but no journal");
                continue;
            };
            let m = self.notes.entry(*id).or_insert((0, 0, BTreeMap::new()));
            let fresh = m.0 != log.epoch();
            let since = if fresh { Since::TooOld } else { log.since(m.1) };
            match since {
                Since::Current => {}
                Since::Changed(ids) => {
                    for o in ids {
                        match canvas.objects.get(&o) {
                            Some(obj) => {
                                m.2.insert(o, obj.clone());
                            }
                            None => {
                                m.2.remove(&o);
                            }
                        }
                    }
                }
                Since::TooOld => m.2 = canvas.objects.clone(),
            }
            m.0 = log.epoch();
            m.1 = log.rev();
            prop_assert_eq!(&m.2, &canvas.objects, "note {}", id);
        }
        Ok(())
    }
}

type Cmd = (u8, usize, f64);

fn apply(e: &mut Engine, (k, i, x): Cmd) {
    let notes: Vec<u64> = e.session.ws.notes.keys().map(|n| n.0).collect();
    let pick = |v: &[u64]| v.get(i % v.len().max(1)).copied().unwrap_or(9_999);
    let objs: Vec<u64> = e
        .session
        .ws
        .canvases
        .values()
        .flat_map(|c| c.objects.keys().map(|o| o.0))
        .collect();
    let _ = match k {
        0 => e.execute("workspace.note.create", json!({"name": "n"})),
        1..=4 => e.execute(
            "ink.stroke.add",
            json!({"note": pick(&notes), "samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 3.0, "y": 2.0, "p": 0.5}]}),
        ),
        5 => e.execute("edit.undo", json!({"scope": pick(&notes)})),
        6 => e.execute("edit.redo", json!({"scope": pick(&notes)})),
        7 => e.execute("edit.undo", json!({"scope": "workspace"})),
        8 => e.execute("workspace.note.delete", json!({"id": pick(&notes)})),
        9 => e.execute("trash.empty", json!({})),
        10 => e.execute("object.delete", json!({"note": pick(&notes), "ids": [pick(&objs)]})),
        11 => e.execute("note.open", json!({"id": pick(&notes), "viewport": [x.abs() + 1.0, 40.0]})),
        _ => {
            e.pump(i);
            Ok(serde_json::Value::Null)
        }
    };
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn journals_reproduce_every_canvas(a in prop::collection::vec((0u8..13, 0usize..6, -2e3f64..2e3), 0..40),
                                       b in prop::collection::vec((0u8..13, 0usize..6, -2e3f64..2e3), 0..40)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        let mut e = Engine::open(&path).unwrap();
        let mut m = Mirror::default();
        for c in &a {
            apply(&mut e, *c);
            m.check(&e)?;
        }
        drop(e);
        // Lazily reopened: pages merged in the background are journalled too.
        let mut e = Engine::open(&path).unwrap();
        let mut m = Mirror::default();
        for c in &b {
            apply(&mut e, *c);
            m.check(&e)?;
        }
        e.wait_loaded();
        m.check(&e)?;
    }
}

#[test]
fn recreating_a_note_starts_a_new_epoch() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "n"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let first = e.session.change_log(NoteId(n)).unwrap().epoch();
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert!(e.session.change_log(NoteId(n)).is_none());
    e.execute("edit.redo", json!({"scope": "workspace"})).unwrap();
    assert_ne!(e.session.change_log(NoteId(n)).unwrap().epoch(), first);
}
