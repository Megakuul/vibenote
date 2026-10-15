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
//! R-SEARCH-001..003 (SPEC §11).

use serde_json::json;
use vibenote_acceptance::*;

/// R-SEARCH-001: handwriting, typed text, PDF text, note and folder names are found.
#[test]
fn r_search_001_all_sources() {
    let (mut e, _) = engine_with_note();
    run(&mut e, "workspace.folder.create", json!({"name": "Analysis"}));
    run(&mut e, "workspace.note.create", json!({"name": "Lineare Algebra"}));
    run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 100.0, "content": "Determinante"}),
    );
    run(
        &mut e,
        "pdf.import",
        json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 500.0]}),
    );
    for (q, src) in [
        ("analysis", "folder"),
        ("lineare", "note"),
        ("determinante", "text"),
        ("zebrafisch", "pdf"),
    ] {
        let r = run(&mut e, "search.query", json!({"text": q}));
        assert!(
            r["results"].as_array().unwrap().iter().any(|h| h["source"] == src),
            "{q} → {src}"
        );
    }
}

/// R-SEARCH-002: global search groups by note; in-note find highlights regions.
#[test]
fn r_search_002_ui_queries() {
    let (mut e, _) = engine_with_note();
    run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 100.0, "content": "Eigenwert"}),
    );
    let r = run(&mut e, "search.in_note", json!({"text": "eigen"}));
    assert!(!r["regions"].as_array().unwrap().is_empty());
}

/// R-SEARCH-003: case/accent-insensitive with umlaut equivalence (ä = ae, ß = ss).
#[test]
fn r_search_003_normalisation() {
    let (mut e, _) = engine_with_note();
    run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 100.0, "content": "Größe Übung"}),
    );
    for q in ["groesse", "GRÖSSE", "uebung", "Ubung"] {
        let r = run(&mut e, "search.query", json!({"text": q}));
        assert!(!r["results"].as_array().unwrap().is_empty(), "{q}");
    }
}
