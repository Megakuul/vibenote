#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Z-order (M4.8, R-CANVAS-005) and note backgrounds (R-CANVAS-006).

use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{NoteId, ObjectId};

fn stroke(e: &mut Engine, n: u64, x: f64) -> u64 {
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 10.0, "y": 0.0, "p": 0.5, "t_us": 1000}]}),
    )
    .unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn open(path: &std::path::Path) -> (Engine, u64) {
    let mut e = Engine::open(path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "z"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    (e, n)
}

/// Object ids bottom to top.
fn stack(e: &Engine, n: u64) -> Vec<u64> {
    let c = e.session.ws.canvas(NoteId(n)).unwrap();
    let mut v: Vec<_> = c.objects.values().map(|o| (o.z, o.id.0)).collect();
    v.sort_unstable();
    v.into_iter().map(|(_, id)| id).collect()
}

#[test]
fn raise_and_lower_keep_the_moved_objects_in_their_order_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let (mut e, n) = open(&dir.path().join("ws.sqlite"));
    let ids: Vec<u64> = (0..5).map(|k| stroke(&mut e, n, f64::from(k) * 20.0)).collect();
    let [a, b, c, d, f] = ids[..] else { panic!() };
    e.execute("object.raise", json!({"ids": [c, a]})).unwrap();
    assert_eq!(stack(&e, n), vec![b, d, f, a, c]);
    e.execute("object.lower", json!({"ids": [f, d, d]})).unwrap();
    assert_eq!(stack(&e, n), vec![d, f, b, a, c]);
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(stack(&e, n), vec![b, d, f, a, c]);
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(stack(&e, n), ids);
    // New ink lands on top of raised objects.
    e.execute("object.raise", json!({"ids": [a]})).unwrap();
    let g = stroke(&mut e, n, 200.0);
    assert_eq!(stack(&e, n).last(), Some(&g));
    // Unknown ids fail and change nothing.
    let before = stack(&e, n);
    assert!(e.execute("object.raise", json!({"ids": [a, 999_999]})).is_err());
    assert!(e.execute("object.lower", json!({"ids": [999_999]})).is_err());
    assert_eq!(stack(&e, n), before);
    // The order survives a reopen.
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    e.load_all().unwrap();
    assert_eq!(stack(&e, n), before);
}

#[test]
fn raising_in_a_partially_loaded_note_goes_above_unloaded_objects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let (mut e, n) = open(&path);
    let near = stroke(&mut e, n, 0.0);
    let far: Vec<u64> = (0..20)
        .map(|k| stroke(&mut e, n, 1.0e6 + f64::from(k) * 20.0))
        .collect();
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": n, "viewport": [100.0, 100.0]}))
        .unwrap();
    e.execute("object.raise", json!({"ids": [near]})).unwrap();
    e.load_all().unwrap();
    let s = stack(&e, n);
    assert_eq!(s.last(), Some(&near));
    assert_eq!(s.len(), far.len() + 1);
    // Lowering completes the note first: below everything, loaded or not.
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": n, "viewport": [100.0, 100.0]}))
        .unwrap();
    e.execute("object.lower", json!({"ids": [near]})).unwrap();
    assert_eq!(stack(&e, n).first(), Some(&near));
    let z = |e: &Engine, id: u64| e.session.ws.canvas(NoteId(n)).unwrap().objects[&ObjectId(id)].z;
    assert!(far.iter().all(|&f| z(&e, f) > z(&e, near)));
}

fn bg(e: &Engine) -> Value {
    e.inspect(Some("/background")).unwrap()
}

#[test]
fn backgrounds_are_per_note_undoable_and_stored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let (mut e, a) = open(&path);
    assert_eq!(bg(&e), json!({"kind": "blank", "spacing": 32.0}));
    e.execute("canvas.set_background", json!({"kind": "grid", "spacing": 40.0}))
        .unwrap();
    assert_eq!(bg(&e), json!({"kind": "grid", "spacing": 40.0}));
    // Omitted fields keep the note's value.
    e.execute("canvas.set_background", json!({"kind": "dots"})).unwrap();
    assert_eq!(bg(&e), json!({"kind": "dots", "spacing": 40.0}));
    // Out of range: rejected, nothing changes.
    for s in [0.0, 3.9, 1000.5, 1.0e300, -8.0] {
        assert!(
            e.execute("canvas.set_background", json!({"spacing": s})).is_err(),
            "{s}"
        );
    }
    assert!(e.execute("canvas.set_background", json!({"kind": "hexagons"})).is_err());
    assert_eq!(bg(&e), json!({"kind": "dots", "spacing": 40.0}));
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(bg(&e), json!({"kind": "grid", "spacing": 40.0}));
    // Another note keeps the workspace default; the default comes from settings.
    let b = e.execute("workspace.note.create", json!({"name": "b"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": b})).unwrap();
    assert_eq!(bg(&e), json!({"kind": "blank", "spacing": 32.0}));
    e.execute("settings.set", json!({"background": {"kind": "lined"}}))
        .unwrap();
    assert_eq!(bg(&e), json!({"kind": "lined", "spacing": 32.0}));
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a})).unwrap();
    assert_eq!(bg(&e), json!({"kind": "grid", "spacing": 40.0}));
    e.execute("note.open", json!({"id": b})).unwrap();
    assert_eq!(bg(&e), json!({"kind": "lined", "spacing": 32.0}));
}

#[test]
fn hostile_stored_backgrounds_fall_back_to_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let (mut e, a) = open(&path);
    e.execute("canvas.set_background", json!({"kind": "grid"})).unwrap();
    e.sync().unwrap();
    drop(e);
    let c = rusqlite::Connection::open(&path).unwrap();
    for bad in ["{\"kind\":\"grid\",\"spacing\":1e300}", "not json", "{\"kind\":7}"] {
        c.execute("UPDATE note SET background = ?1", [bad]).unwrap();
        let mut e = Engine::open(&path).unwrap();
        e.execute("note.open", json!({"id": a})).unwrap();
        assert_eq!(bg(&e), json!({"kind": "blank", "spacing": 32.0}), "{bad}");
    }
}

/// Review M4 H1: a background change and later note edits (rename, move) undo independently.
#[test]
fn background_undo_never_touches_name_or_folder_and_vice_versa() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "a"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let f = e.execute("workspace.folder.create", json!({"name": "F"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    e.execute("canvas.set_background", json!({"kind": "grid"})).unwrap();
    e.execute("workspace.note.rename", json!({"id": n, "name": "renamed"}))
        .unwrap();
    e.execute("workspace.note.move", json!({"id": n, "folder": f})).unwrap();
    let note = |e: &Engine| e.inspect(Some(&format!("/ws/notes/{n}"))).unwrap();
    // Undo in the note scope: only the background goes.
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(note(&e)["name"], json!("renamed"));
    assert_eq!(note(&e)["folder"], json!(f));
    assert_eq!(bg(&e)["kind"], json!("blank"));
    e.execute("edit.redo", json!({})).unwrap();
    // Undo in the workspace scope: move and rename go, the background stays.
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert_eq!(note(&e)["name"], json!("a"));
    assert_eq!(bg(&e)["kind"], json!("grid"));
    // Following the default again, and a no-op call, behave.
    let steps = |e: &Engine| {
        e.inspect(Some(&format!("/history/note:{n}/undo")))
            .map(|v| v.as_array().map_or(0, Vec::len))
            .unwrap_or(0)
    };
    let before = steps(&e);
    e.execute("canvas.set_background", json!({})).unwrap();
    assert_eq!(steps(&e), before, "nothing changed: no undo step");
    e.execute("canvas.set_background", json!({"default": true})).unwrap();
    e.execute("settings.set", json!({"background": {"kind": "dots"}}))
        .unwrap();
    assert_eq!(bg(&e)["kind"], json!("dots"));
    assert!(
        e.execute("canvas.set_background", json!({"default": true, "kind": "grid"}))
            .is_err()
    );
}
