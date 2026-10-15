#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Persistent per-scope undo/redo (M2.2, R-UNDO-001): history survives reopening and behaves
//! exactly like an uninterrupted session.

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::{Engine, UNDO_LIMIT};

type Cmd = (u8, usize, f64);

fn apply(e: &mut Engine, (k, i, x): Cmd) {
    let notes: Vec<u64> = e.session.ws.notes.keys().map(|n| n.0).collect();
    let pick = |v: &Vec<u64>| v.get(i % v.len().max(1)).copied().unwrap_or(9_999);
    let _ = match k {
        0 => e.execute("workspace.folder.create", json!({"name": format!("f{i}")})),
        1 => e.execute("workspace.note.create", json!({"name": "n"})),
        2 | 3 => e.execute(
            "ink.stroke.add",
            json!({"note": pick(&notes), "samples": [{"x": x, "y": -x, "p": 0.5}, {"x": x + 3.3, "y": 1.0, "p": 0.7, "t_us": 9000}]}),
        ),
        4 => e.execute("edit.undo", json!({"scope": pick(&notes)})),
        5 => e.execute("edit.redo", json!({"scope": pick(&notes)})),
        6 => e.execute("edit.undo", json!({"scope": "workspace"})),
        7 => e.execute("edit.redo", json!({"scope": "workspace"})),
        _ => e.execute("workspace.note.delete", json!({"id": pick(&notes)})),
    };
}

fn cmds() -> impl Strategy<Value = Vec<Cmd>> {
    prop::collection::vec((0u8..9, 0usize..4, -1e4f64..1e4), 0..30)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]
    /// A, close, reopen, B  ≡  A, B in one in-memory session (state and every history).
    #[test]
    fn reopen_keeps_undo_redo_behaviour(a in cmds(), b in cmds()) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        let mut mem = Engine::new().unwrap();
        let mut disk = Engine::open(&path).unwrap();
        for c in &a {
            apply(&mut mem, *c);
            apply(&mut disk, *c);
        }
        drop(disk);
        let mut disk = Engine::open(&path).unwrap();
        for c in &b {
            apply(&mut mem, *c);
            apply(&mut disk, *c);
        }
        disk.load_all().unwrap();
        prop_assert_eq!(&disk.session.ws, &mem.session.ws);
        let keys: Vec<String> = mem.session.history.keys().cloned().collect();
        for k in keys {
            let want = mem.session.history_of(&k);
            prop_assert_eq!(disk.session.history_of(&k), want, "scope {}", k);
        }
        // And once more from disk only.
        drop(disk);
        let mut back = Engine::open(&path).unwrap();
        for (k, h) in mem.session.history.clone() {
            prop_assert_eq!(back.session.history_of(&k), h, "scope {} after reopen", k);
        }
    }
}

fn stroke(e: &mut Engine, n: u64, y: f64) {
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": 0.0, "y": y, "p": 0.5}, {"x": 1.0, "y": y, "p": 0.5, "t_us": 1000}]}),
    )
    .unwrap();
}

#[test]
fn bound_of_1000_steps_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "b"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    for i in 0..1005 {
        stroke(&mut e, n, f64::from(i));
    }
    drop(e);
    let st = vibenote_storage::Store::open(&path).unwrap();
    assert_eq!(st.load_history(&format!("note:{n}")).unwrap().undo.len(), UNDO_LIMIT);
    drop(st);
    let mut e = Engine::open(&path).unwrap();
    let mut undone = 0;
    while e.execute("edit.undo", json!({"scope": n})).is_ok() {
        undone += 1;
    }
    assert_eq!(undone, UNDO_LIMIT);
    assert_eq!(e.session.ws.canvas(vibenote_model::NoteId(n)).unwrap().objects.len(), 5);
}

/// Opening a workspace loads no history; a scope is read on first use only.
#[test]
fn history_loads_lazily() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "l"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    stroke(&mut e, n, 0.0);
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(e.inspect(Some("/history")).unwrap(), json!({}));
    e.execute("note.open", json!({"id": n})).unwrap();
    let h: Value = e.inspect(Some(&format!("/history/note:{n}"))).unwrap();
    assert_eq!(h["undo"].as_array().unwrap().len(), 1, "note.open preloads its history");
    e.execute("edit.undo", json!({})).unwrap();
    let h: Value = e.inspect(Some(&format!("/history/note:{n}"))).unwrap();
    assert_eq!(h["undo"].as_array().unwrap().len(), 0);
    assert_eq!(h["redo"].as_array().unwrap().len(), 1);
    assert!(
        e.inspect(Some("/history/workspace")).is_err(),
        "workspace scope not loaded"
    );
}
