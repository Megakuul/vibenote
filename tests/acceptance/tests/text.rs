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
//! R-TEXT-001..003 (SPEC §5).

use serde_json::json;
use vibenote_acceptance::*;

/// R-TEXT-001: basic rich text formatting survives save/load.
#[test]
fn r_text_001_rich_text() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "t"})));
    run(&mut e, "note.open", json!({"id": n}));
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": "Überschrift Liste"}),
    ));
    for (fmt, range) in [
        ("bold", [0, 11]),
        ("italic", [0, 4]),
        ("underline", [5, 11]),
        ("strike", [12, 17]),
        ("heading1", [0, 11]),
        ("bullet_list", [12, 17]),
        ("checkbox", [12, 17]),
    ] {
        run(&mut e, "text.format", json!({"id": t, "format": fmt, "range": range}));
    }
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = vibenote_engine::Engine::open(&db).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
}

/// R-TEXT-002: German characters typed via IME/xkb arrive unchanged.
#[test]
fn r_text_002_umlauts() {
    let (mut e, _) = engine_with_note();
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": ""}),
    ));
    run(&mut e, "text.edit", json!({"id": t, "insert": "ÄÖÜäöüß", "at": 0}));
    assert_eq!(run(&mut e, "text.get", json!({"id": t}))["content"], "ÄÖÜäöüß");
}

/// R-TEXT-003: typed text is searchable.
#[test]
fn r_text_003_searchable() {
    let (mut e, _) = engine_with_note();
    run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": "Eigenwert Matrix"}),
    );
    let r = run(&mut e, "search.query", json!({"text": "eigenwert"}));
    assert!(!r["results"].as_array().unwrap().is_empty());
}
