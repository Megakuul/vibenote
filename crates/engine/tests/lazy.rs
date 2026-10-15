#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
//! Lazy open (M2.3, R-PERF-004): `Engine::open` reads no canvas, `note.open` reads the viewport
//! first and streams the rest. Commands behave exactly as if everything had been loaded.

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::{Engine, LoadState};
use vibenote_model::{NoteId, ObjectKind};

fn stroke(e: &mut Engine, n: u64, x: f64, y: f64) -> Result<Value, vibenote_engine::EngineError> {
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": x, "y": y, "p": 0.5}, {"x": x + 4.0, "y": y + 1.0, "p": 0.6, "t_us": 4000}]}),
    )
}

fn new_note(e: &mut Engine, name: &str) -> u64 {
    e.execute("workspace.note.create", json!({ "name": name })).unwrap()["id"]
        .as_u64()
        .unwrap()
}

/// A workspace on disk with two notes of strokes spread over a large area.
fn populated(per_note: u32) -> (tempfile::TempDir, std::path::PathBuf, [u64; 2]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let notes = [new_note(&mut e, "a"), new_note(&mut e, "b")];
    for k in 0..per_note {
        for n in notes {
            let (x, y) = (f64::from(k % 40) * 150.0, f64::from(k / 40) * 120.0);
            stroke(&mut e, n, x, y).unwrap();
        }
    }
    e.sync().unwrap();
    (dir, path, notes)
}

fn count(e: &Engine, n: u64) -> usize {
    e.session.ws.canvas(NoteId(n)).unwrap().objects.len()
}

#[test]
fn open_reads_no_canvas() {
    let (_d, path, notes) = populated(30);
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.session.ws.notes.len(), 2);
    for n in notes {
        assert_eq!(count(&e, n), 0);
        assert_eq!(e.session.load_state(NoteId(n)), LoadState::Unloaded);
    }
    assert_eq!(
        e.inspect(Some("/loading")).unwrap(),
        json!({"partial": [], "unloaded": 2})
    );
}

#[test]
fn note_open_loads_the_view_first_then_streams_the_rest() {
    let (_d, path, [a, b]) = populated(400);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a, "viewport": [400.0, 300.0]}))
        .unwrap();
    // Area = view (0,0)–(400,300) plus a quarter on each side.
    let first = count(&e, a);
    let inside = e
        .session
        .ws
        .canvas(NoteId(a))
        .unwrap()
        .objects
        .values()
        .all(|o| match &o.kind {
            ObjectKind::Stroke(s) => {
                let r = s.bounds();
                r.max.x >= -100.0 && r.min.x <= 500.0 && r.max.y >= -75.0 && r.min.y <= 375.0
            }
            _ => true,
        });
    assert!(first > 0 && first < 400, "viewport only: {first}");
    assert!(inside);
    assert_eq!(e.session.load_state(NoteId(a)), LoadState::Partial);
    assert_eq!(e.inspect(Some("/loading/partial")).unwrap(), json!([a]));
    e.wait_loaded();
    assert_eq!(count(&e, a), 400);
    assert_eq!(e.session.load_state(NoteId(a)), LoadState::Complete);
    assert_eq!(count(&e, b), 0, "other notes stay unloaded");
    assert!(!e.loading());
}

#[test]
fn pump_merges_pages_without_waiting() {
    let (_d, path, [a, _]) = populated(5000);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a, "viewport": [100.0, 100.0]}))
        .unwrap();
    let mut merged = 0;
    let t = std::time::Instant::now();
    while e.loading() {
        merged += e.pump(1);
        assert!(t.elapsed().as_secs() < 30, "load did not finish");
        std::thread::yield_now();
    }
    assert!(merged >= 3, "5000 objects come in several pages ({merged})");
    assert_eq!(count(&e, a), 5000);
}

/// Strokes drawn before the rest of the note arrives go above every stored object and are
/// neither lost nor doubled when the stream catches up.
#[test]
fn strokes_while_loading_go_on_top() {
    let (_d, path, [a, _]) = populated(300);
    let stored_max = {
        let mut e = Engine::open(&path).unwrap();
        e.load_all().unwrap();
        e.session
            .ws
            .canvas(NoteId(a))
            .unwrap()
            .objects
            .values()
            .map(|o| o.z)
            .max()
            .unwrap()
    };
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a, "viewport": [50.0, 50.0]}))
        .unwrap();
    let id = stroke(&mut e, a, 1.0, 1.0).unwrap()["id"].as_u64().unwrap();
    let z = e.session.ws.canvas(NoteId(a)).unwrap().objects[&vibenote_model::ObjectId(id)].z;
    assert_eq!(z, stored_max + 1);
    e.sync().unwrap(); // the stream may now read the new stroke from disk as well
    e.wait_loaded();
    assert_eq!(count(&e, a), 301);
}

/// Deleting a note that is not (fully) loaded keeps its whole canvas for undo.
#[test]
fn deleting_an_incomplete_note_keeps_its_canvas() {
    let (_d, path, [a, b]) = populated(200);
    let full = {
        let mut e = Engine::open(&path).unwrap();
        e.load_all().unwrap();
        e.session.ws.clone()
    };
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a, "viewport": [50.0, 50.0]}))
        .unwrap();
    e.execute("workspace.note.delete", json!({"id": a})).unwrap();
    e.execute("workspace.note.delete", json!({"id": b})).unwrap();
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    e.load_all().unwrap();
    assert_eq!(e.session.ws, full);
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.load_all().unwrap();
    assert_eq!(e.session.ws, full, "and on disk");
}

#[test]
fn hostile_viewports_are_rejected() {
    let (_d, path, [a, _]) = populated(1);
    let mut e = Engine::open(&path).unwrap();
    for v in [
        json!([0.0, 10.0]),
        json!([-1.0, 10.0]),
        json!([1e300, 10.0]),
        json!([10.0]),
    ] {
        assert!(e.execute("note.open", json!({"id": a, "viewport": v})).is_err(), "{v}");
    }
    assert_eq!(e.session.current_note, None);
    assert_eq!(e.session.load_state(NoteId(a)), LoadState::Unloaded);
}

type Cmd = (u8, usize, f64, f64);

/// Runs one command; the result is compared between engines (load state must not show).
fn apply(e: &mut Engine, ids: &[u64], (k, i, x, y): Cmd) -> Result<Value, String> {
    let notes: Vec<u64> = e.session.ws.notes.keys().map(|n| n.0).collect();
    let pick = |v: &[u64]| v.get(i % v.len().max(1)).copied().unwrap_or(9_999);
    let r = match k {
        0 => e.execute("workspace.note.create", json!({"name": "n"})),
        1..=3 => stroke(e, pick(&notes), x, y),
        4 => e.execute("edit.undo", json!({"scope": pick(&notes)})),
        5 => e.execute("edit.redo", json!({"scope": pick(&notes)})),
        6 => e.execute("edit.undo", json!({"scope": "workspace"})),
        7 => e.execute("edit.redo", json!({"scope": "workspace"})),
        8 => e.execute("workspace.note.delete", json!({"id": pick(&notes)})),
        9 => e.execute("object.delete", json!({"note": pick(&notes), "ids": [pick(ids)]})),
        10..=11 => e.execute(
            "note.open",
            json!({"id": pick(&notes), "viewport": [x.abs() + 1.0, y.abs() + 1.0]}),
        ),
        _ => {
            e.pump(i);
            Ok(Value::Null)
        }
    };
    r.map_err(|e| e.to_string())
}

fn cmds(n: usize) -> impl Strategy<Value = Vec<Cmd>> {
    prop::collection::vec((0u8..13, 0usize..8, -3e3f64..3e3, -3e3f64..3e3), 0..n)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    /// A, close, reopen (lazy), B  ≡  A, B in one in-memory session: same result for every
    /// command, same state and history once everything is loaded.
    #[test]
    fn lazy_session_behaves_like_a_loaded_one(a in cmds(60), b in cmds(40)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        let mut mem = Engine::new().unwrap();
        let mut disk = Engine::open(&path).unwrap();
        for c in &a {
            let ids: Vec<u64> = all_ids(&mem);
            prop_assert_eq!(apply(&mut disk, &ids, *c), apply(&mut mem, &ids, *c));
        }
        drop(disk);
        let mut disk = Engine::open(&path).unwrap();
        for c in &b {
            let ids: Vec<u64> = all_ids(&mem);
            let (d, m) = (apply(&mut disk, &ids, *c), apply(&mut mem, &ids, *c));
            prop_assert_eq!(d, m, "command {:?}", c);
        }
        disk.load_all().unwrap();
        prop_assert_eq!(&disk.session.ws, &mem.session.ws);
        let keys: Vec<String> = mem.session.history.keys().cloned().collect();
        for k in keys {
            let want = mem.session.history_of(&k);
            prop_assert_eq!(disk.session.history_of(&k), want, "scope {}", k);
        }
    }
}

fn all_ids(e: &Engine) -> Vec<u64> {
    e.session
        .ws
        .canvases
        .values()
        .flat_map(|c| c.objects.keys().map(|o| o.0))
        .collect()
}

/// Loads until no note is partial, failing after 30 s (a stuck load must not hang the test).
fn settle(e: &mut Engine) {
    let t = std::time::Instant::now();
    while e.loading() {
        e.pump(usize::MAX);
        assert!(
            t.elapsed().as_secs() < 30,
            "a note stayed partial: {:?}",
            e.inspect(Some("/loading"))
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Regression (M2 review H1): opening a second note while the first streams used to abandon
/// the first, leaving it partial for the session.
#[test]
fn opening_another_note_keeps_loading_the_first() {
    let (_d, path, [a, b]) = populated(5000);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a, "viewport": [50.0, 50.0]}))
        .unwrap();
    e.execute("note.open", json!({"id": b, "viewport": [50.0, 50.0]}))
        .unwrap();
    settle(&mut e);
    for n in [a, b] {
        assert_eq!(count(&e, n), 5000);
        assert_eq!(e.session.load_state(NoteId(n)), LoadState::Complete);
    }
}

/// Regression (M2 review M2): undoing a stroke that is in memory does not read the rest of the
/// note, and the stroke does not come back when the rest arrives, even though its row is
/// still on disk while the removal waits for the write lock.
#[test]
fn undo_on_a_partial_note_is_local_and_exact() {
    let (_d, path, [a, _]) = populated(5000);
    let mut e = Engine::open(&path).unwrap();
    // The stroke is in memory and on disk before the note's stream starts reading, so the
    // stream sees its row; the removal then waits behind another writer's lock.
    let id = stroke(&mut e, a, 2.0, 2.0).unwrap()["id"].as_u64().unwrap();
    e.sync().unwrap();
    let lock = rusqlite::Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    e.execute("note.open", json!({"id": a, "viewport": [50.0, 50.0]}))
        .unwrap();
    e.execute("edit.undo", json!({"scope": a})).unwrap();
    assert_eq!(e.session.load_state(NoteId(a)), LoadState::Partial, "no full read");
    settle(&mut e);
    let objs = &e.session.ws.canvas(NoteId(a)).unwrap().objects;
    assert!(
        !objs.contains_key(&vibenote_model::ObjectId(id)),
        "undone stroke came back"
    );
    assert_eq!(objs.len(), 5000);
    lock.execute_batch("ROLLBACK").unwrap();
    e.execute("edit.redo", json!({"scope": a})).unwrap();
    e.load_all().unwrap(); // note b was never opened
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = Engine::open(&path).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
}

/// Regression (M2 review L1): an unreadable stored history is cleared on disk as well, so its
/// stale steps cannot return after a reopen.
#[test]
fn unreadable_history_is_cleared() {
    let (_d, path, [a, _]) = populated(3);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute("UPDATE history SET ops = x'00' WHERE scope = ?1", [format!("note:{a}")])
        .unwrap();
    drop(c);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": a})).unwrap();
    assert!(e.inspect(Some("/warnings")).is_ok());
    stroke(&mut e, a, 1.0, 1.0).unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(e.session.history_of(&format!("note:{a}")).undo.len(), 1);
}
