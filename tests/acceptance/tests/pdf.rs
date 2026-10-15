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
//! R-PDF-001..005 (SPEC §4).

use serde_json::json;
use vibenote_acceptance::*;

fn pdf() -> std::path::PathBuf {
    root().join("tests/fixtures/pdf/three-pages.pdf")
}

/// R-PDF-001: one object, pages stacked vertically with a fixed gap.
#[test]
fn r_pdf_001_vertical_column() {
    let (mut e, n) = engine_with_note();
    let r = run(&mut e, "pdf.import", json!({"path": pdf(), "at": [0.0, 0.0]}));
    let o = objects(&e, n);
    assert_eq!(o.len(), 1);
    assert_eq!(o[0]["kind"], "pdf");
    let pages = r["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3);
    let gap = r["gap"].as_f64().unwrap();
    for w in pages.windows(2) {
        let bottom = w[0]["y"].as_f64().unwrap() + w[0]["height"].as_f64().unwrap();
        assert!((w[1]["y"].as_f64().unwrap() - (bottom + gap)).abs() < 1e-9);
    }
}

/// R-PDF-002: detach page 2 of 3 → two objects; undo restores.
#[test]
fn r_pdf_002_detach_page() {
    let (mut e, n) = engine_with_note();
    let id = id(&run(&mut e, "pdf.import", json!({"path": pdf(), "at": [0.0, 0.0]})));
    let before = objects(&e, n);
    run(&mut e, "pdf.page_detach", json!({"id": id, "page": 2}));
    assert_eq!(objects(&e, n).len(), 2);
    run(&mut e, "edit.undo", json!({}));
    assert_eq!(objects(&e, n), before);
}

/// R-PDF-003 / R-PERF-006: lazy page rendering within budget.
#[test]
fn r_pdf_003_lazy_pages_within_budget() {
    let (mut e, _) = engine_with_note();
    let id = id(&run(&mut e, "pdf.import", json!({"path": pdf(), "at": [0.0, 0.0]})));
    let t = std::time::Instant::now();
    run(
        &mut e,
        "pdf.render_page",
        json!({"id": id, "page": 3, "width_px": 1240}),
    );
    assert!(t.elapsed().as_secs_f64() * 1e3 <= budget("pdf", "page_visible_ms"));
}

/// R-PDF-004: PDF text is searchable (page 2 contains "Zebrafisch").
#[test]
fn r_pdf_004_text_searchable() {
    let (mut e, _) = engine_with_note();
    run(&mut e, "pdf.import", json!({"path": pdf(), "at": [0.0, 0.0]}));
    let r = run(&mut e, "search.query", json!({"text": "zebrafisch"}));
    assert!(
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["source"] == "pdf" && h["page"] == 2)
    );
}

/// R-PDF-005: stored PDF bytes are identical to the imported file.
#[test]
fn r_pdf_005_original_bytes_kept() {
    let (mut e, _) = engine_with_note();
    let r = run(&mut e, "pdf.import", json!({"path": pdf(), "at": [0.0, 0.0]}));
    let blob = run(&mut e, "blob.read", json!({"hash": r["blob"]}));
    let stored: Vec<u8> = serde_json::from_value(blob["bytes"].clone()).unwrap();
    assert_eq!(stored, std::fs::read(pdf()).unwrap());
}
