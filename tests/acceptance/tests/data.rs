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
//! R-DATA-001..004 (SPEC §13). The kill -9 loop lives in apps/vibenote-cli/tests/crash.rs.

use serde_json::json;
use vibenote_acceptance::*;

/// R-DATA-001: a stroke is durable within 1 s of pen-up (visible to an independent reader).
#[test]
fn r_data_001_durable_after_pen_up() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "d"})));
    run(&mut e, "note.open", json!({"id": n}));
    let f = fixture("lines-5.jsonl");
    let up = f
        .events
        .iter()
        .position(|x| x.kind == vibenote_ink::input::PenEventKind::Up)
        .unwrap();
    let t = std::time::Instant::now();
    run(&mut e, "input.pen", json!({"events": &f.events[..=up]}));
    let reader = vibenote_storage::Store::open(&db).unwrap();
    let mut seen = false;
    while t.elapsed().as_secs_f64() * 1e3 <= budget("durability", "pen_up_to_durable_ms") {
        let ws = reader.load().unwrap();
        if ws.canvases.values().any(|c| !c.objects.is_empty()) {
            seen = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(seen, "stroke not durable within the budget");
}

/// R-DATA-002: reference workload opens (scale smoke test; budgets in perf.rs).
#[test]
fn r_data_002_reference_workload_opens() {
    let (db, note) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    e.load_all().unwrap();
    assert_eq!(e.session.ws.notes.len(), 10_001);
    assert_eq!(e.session.ws.canvas(note).unwrap().objects.len(), 100_000);
}

/// R-DATA-003: versioned format; save→load identity (property tests in crates/storage).
#[test]
fn r_data_003_versioned_format() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    drop(vibenote_storage::Store::open(&db).unwrap());
    let c = vibenote_storage::Store::open(&db).unwrap();
    assert_eq!(c.integrity_check().unwrap(), "ok");
    assert_eq!(
        c.schema_version().unwrap(),
        vibenote_storage::SCHEMA_VERSION,
        "database records its format version"
    );
}

/// R-DATA-004: corrupt workspace files are errors, not crashes.
#[test]
fn r_data_004_corrupt_workspace_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    std::fs::write(&db, b"this is not a database at all").unwrap();
    assert!(vibenote_engine::Engine::open(&db).is_err());
}

/// R-DATA-004: hostile PDFs/images/audio are rejected without crashing.
#[test]
fn r_data_004_hostile_imports() {
    let (mut e, _) = engine_with_note();
    for (cmd, f) in [("pdf.import", "corrupt.pdf"), ("pdf.import", "truncated.pdf")] {
        let r = try_run(
            &mut e,
            cmd,
            json!({"path": root().join("tests/fixtures/pdf").join(f), "at": [0.0, 0.0]}),
        );
        assert!(
            matches!(
                r,
                Err(vibenote_engine::EngineError::Model(_)) | Err(vibenote_engine::EngineError::Storage(_))
            ),
            "{cmd} {f}: {r:?}"
        );
    }
}
