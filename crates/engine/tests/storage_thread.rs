#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! Storage thread (M2.1, R-PERF-009 / R-DATA-001): commands never wait for the disk, every
//! change becomes durable, commit errors surface and are retried without losing ops.

use serde_json::json;
use std::time::{Duration, Instant};
use vibenote_engine::Engine;

fn stroke(e: &mut Engine, x: f64) -> u64 {
    e.execute(
        "ink.stroke.add",
        json!({"samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 4.0, "y": 2.0, "p": 0.5, "t_us": 3000}]}),
    )
    .unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&db).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "s"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    (dir, db, e)
}

fn stored_objects(db: &std::path::Path) -> usize {
    let ws = vibenote_storage::Store::open(db).unwrap().load().unwrap();
    ws.canvases.values().map(|c| c.objects.len()).sum()
}

/// Another process holds the write lock for 400 ms: inking continues without waiting, and
/// everything lands once the lock is released.
#[test]
fn commands_do_not_wait_for_a_locked_database() {
    let (_d, db, mut e) = setup();
    e.sync().unwrap();
    let before = e.storage_status().unwrap().durable;
    let other = rusqlite::Connection::open(&db).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let t = Instant::now();
    let mut worst = Duration::ZERO;
    for i in 0..50 {
        let s = Instant::now();
        stroke(&mut e, f64::from(i) * 10.0);
        worst = worst.max(s.elapsed());
    }
    assert!(t.elapsed() < Duration::from_millis(300), "inking waited for storage");
    assert!(worst < Duration::from_millis(50), "worst command {worst:?}");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        e.storage_status().unwrap().durable,
        before,
        "nothing past the lock is durable yet"
    );
    other.execute_batch("COMMIT").unwrap();
    e.sync().unwrap();
    let st = e.storage_status().unwrap();
    assert_eq!(st.durable, st.submitted);
    assert!(st.commits < st.submitted, "queued batches were grouped: {st:?}");
    assert_eq!(stored_objects(&db), 50);
}

/// A failing commit is reported (sync + status), retried, and nothing is lost or reordered.
#[test]
fn commit_errors_surface_and_are_retried() {
    let (_d, db, mut e) = setup();
    e.sync().unwrap();
    let other = rusqlite::Connection::open(&db).unwrap();
    other
        .execute_batch("CREATE TRIGGER fail BEFORE INSERT ON object BEGIN SELECT RAISE(ABORT, 'disk says no'); END;")
        .unwrap();
    let a = stroke(&mut e, 0.0);
    let err = e.sync().unwrap_err().to_string();
    assert!(err.contains("disk says no"), "{err}");
    assert!(e.storage_status().unwrap().error.is_some());
    assert!(e.inspect(Some("/storage/error")).unwrap().is_string());
    let b = stroke(&mut e, 50.0);
    e.execute("edit.undo", json!({})).unwrap();
    e.execute("edit.redo", json!({})).unwrap();
    other.execute_batch("DROP TRIGGER fail").unwrap();
    let t = Instant::now();
    while e.sync().is_err() {
        assert!(t.elapsed() < Duration::from_secs(10), "never recovered");
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = e.storage_status().unwrap();
    assert_eq!((st.durable, st.error), (st.submitted, None));
    let ws = vibenote_storage::Store::open(&db).unwrap().load().unwrap();
    let c = ws.canvases.values().next().unwrap();
    assert!(c.objects.contains_key(&vibenote_model::ObjectId(a)));
    assert!(c.objects.contains_key(&vibenote_model::ObjectId(b)));
    assert_eq!(ws, e.session.ws);
}

/// Dropping the engine finishes queued commits (reopen sees everything, no explicit sync).
#[test]
fn drop_finishes_queued_commits() {
    let (_d, db, mut e) = setup();
    for i in 0..200 {
        stroke(&mut e, f64::from(i));
    }
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = Engine::open(&db).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
}

/// In-memory engines have no storage thread; sync is a no-op.
#[test]
fn in_memory_sync_is_noop() {
    let mut e = Engine::new().unwrap();
    e.execute("workspace.note.create", json!({"name": "m"})).unwrap();
    e.sync().unwrap();
    assert!(e.storage_status().is_none());
    assert!(e.inspect(Some("/storage")).unwrap().is_null());
}
