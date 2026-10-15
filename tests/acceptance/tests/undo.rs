#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! R-UNDO-001..002 (SPEC §12).

use serde_json::json;
use vibenote_acceptance::*;

/// R-UNDO-001: per-note history persists across restarts (bounded to 1,000 steps).
#[test]
fn r_undo_001_persistent_per_note_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "u"})));
    run(&mut e, "note.open", json!({"id": n}));
    let empty = objects(&e, n);
    line(&mut e, n, (0.0, 0.0), (10.0, 0.0), 0.5);
    line(&mut e, n, (0.0, 5.0), (10.0, 5.0), 0.5);
    drop(e);
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    run(&mut e, "note.open", json!({"id": n}));
    run(&mut e, "edit.undo", json!({}));
    run(&mut e, "edit.undo", json!({}));
    assert_eq!(objects(&e, n), empty);
}

/// R-UNDO-001 (bound): history keeps exactly the last 1,000 steps.
#[test]
fn r_undo_001_history_bound() {
    let (mut e, n) = engine_with_note();
    for i in 0..1005 {
        line(&mut e, n, (0.0, f64::from(i)), (1.0, f64::from(i)), 0.5);
    }
    let mut undone = 0;
    while try_run(&mut e, "edit.undo", json!({})).is_ok() {
        undone += 1;
    }
    assert_eq!(undone, 1000);
    assert_eq!(objects(&e, n).len(), 5);
}

/// R-UNDO-002: copy/paste within and between notes.
#[test]
fn r_undo_002_clipboard() {
    let (mut e, n) = engine_with_note();
    let s = line(&mut e, n, (0.0, 0.0), (10.0, 0.0), 0.5);
    run(&mut e, "selection.set", json!({"ids": [s]}));
    run(&mut e, "clipboard.copy", json!({}));
    let other = id(&run(&mut e, "workspace.note.create", json!({"name": "dst"})));
    run(&mut e, "note.open", json!({"id": other}));
    run(&mut e, "clipboard.paste", json!({"at": [100.0, 100.0]}));
    assert_eq!(objects(&e, other).len(), 1);
}
