#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! A disk that refuses the last commits at shutdown loses nothing (M2 review M1): the changes
//! go to a recovery file that the next open applies.

use serde_json::json;
use vibenote_engine::Engine;

fn stroke(e: &mut Engine, x: f64) {
    e.execute(
        "ink.stroke.add",
        json!({"samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 4.0, "y": 2.0, "p": 0.5, "t_us": 3000}]}),
    )
    .unwrap();
}

#[test]
fn failed_shutdown_commits_are_recovered_on_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&db).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "r"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    stroke(&mut e, 0.0);
    e.sync().unwrap();
    let other = rusqlite::Connection::open(&db).unwrap();
    other
        .execute_batch("CREATE TRIGGER fail BEFORE INSERT ON object BEGIN SELECT RAISE(ABORT, 'disk full'); END;")
        .unwrap();
    for k in 1..=5 {
        stroke(&mut e, f64::from(k) * 10.0);
    }
    e.execute("edit.undo", json!({})).unwrap();
    e.execute("hwr.set_mode", json!({"mode": "off"})).unwrap();
    assert!(e.sync().is_err());
    e.load_all().unwrap();
    let mem = e.session.ws.clone();
    let hist = e.session.history_of(&format!("note:{n}"));
    drop(e); // shutdown commits fail too → recovery file
    let rec = dir.path().join("ws.sqlite.recovery");
    assert!(rec.exists(), "recovery file written");
    other.execute_batch("DROP TRIGGER fail").unwrap();
    drop(other);

    let mut back = Engine::open(&db).unwrap();
    assert!(!rec.exists(), "applied and removed");
    let w = back.inspect(Some("/warnings")).unwrap().to_string();
    assert!(w.contains("recovered"), "{w}");
    assert_eq!(back.inspect(Some("/settings/prettify_mode")).unwrap(), json!("off"));
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
    assert_eq!(back.session.history_of(&format!("note:{n}")), hist);
    drop(back);
    // And it is durable now: a further reopen needs no recovery.
    let again = Engine::open(&db).unwrap();
    assert!(again.inspect(Some("/warnings")).is_err());
}

/// A damaged recovery file is kept (never applied half-way) and its ids stay reserved.
#[test]
fn damaged_recovery_file_is_kept_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    drop(Engine::open(&db).unwrap());
    let rec = dir.path().join("ws.sqlite.recovery");
    std::fs::write(&rec, b"VNR1 this is not a valid recovery file").unwrap();
    let e = Engine::open(&db).unwrap();
    let w = e.inspect(Some("/warnings")).unwrap().to_string();
    assert!(w.contains("unreadable (kept)"), "{w}");
    assert!(rec.exists());
}
