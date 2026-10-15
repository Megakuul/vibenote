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
//! R-HWR-001..008 (SPEC §10). Eval data: tests/eval (permissive, committed) — see tests/eval/README.md.

use serde_json::json;
use vibenote_acceptance::*;

fn eval_dir() -> std::path::PathBuf {
    root().join("tests/eval")
}

/// R-HWR-001: block segmentation matches labels (≥ 95 %).
#[test]
fn r_hwr_001_block_segmentation() {
    let labels: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(eval_dir().join("blocks/labels.json")).expect("eval set tests/eval/blocks"),
    )
    .unwrap();
    let (mut e, _) = engine_with_note();
    let r = run(
        &mut e,
        "hwr.segment_blocks",
        json!({"fixture": eval_dir().join("blocks/blocks.jsonl"), "idle_ms": 1500}),
    );
    let ok = r["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(labels["blocks"].as_array().unwrap())
        .filter(|(a, b)| a["strokes"] == b["strokes"])
        .count();
    assert!(ok as f64 >= 0.95 * labels["blocks"].as_array().unwrap().len() as f64);
}

/// R-HWR-002: drawing false positives on the eval set = 0 (hard gate); writing recall ≥ 60 %.
#[test]
fn r_hwr_002_classifier_zero_drawing_fp() {
    let (mut e, _) = engine_with_note();
    let r = run(
        &mut e,
        "hwr.eval_classifier",
        json!({"set": eval_dir().join("writing-drawing")}),
    );
    assert_eq!(r["drawing_false_positives"], 0);
    assert!(r["writing_recall"].as_f64().unwrap() >= 0.60);
}

/// R-HWR-003: CER ≤ 10 % (en), ≤ 12 % (de) on the eval sets (red until improved; ADR 0006).
#[test]
fn r_hwr_003_recognition_accuracy() {
    let (mut e, _) = engine_with_note();
    let r = run(
        &mut e,
        "hwr.eval_recognition",
        json!({"set": eval_dir().join("recognition")}),
    );
    assert!(r["cer_en"].as_f64().unwrap() <= 0.10, "{r}");
    assert!(r["cer_de"].as_f64().unwrap() <= 0.12, "{r}");
}

/// R-HWR-004: prettify bounds (displacement ≤ 0.35·x-height, stroke count/order unchanged).
#[test]
fn r_hwr_004_prettify_bounds() {
    let (mut e, n) = engine_with_note();
    replay(&mut e, &fixture("loops-4.jsonl"));
    let before = objects(&e, n);
    let r = run(
        &mut e,
        "hwr.prettify_block",
        json!({"strokes": before.iter().map(|o| o["id"].clone()).collect::<Vec<_>>(), "force": true}),
    );
    assert!(r["max_shape_displacement_xh"].as_f64().unwrap() <= 0.35);
    assert_eq!(objects(&e, n).len(), before.len());
}

/// R-HWR-005: prettify → revert → strokes byte-identical.
#[test]
fn r_hwr_005_revert_identical() {
    let (mut e, n) = engine_with_note();
    replay(&mut e, &fixture("loops-4.jsonl"));
    let before = objects(&e, n);
    let ids: Vec<_> = before.iter().map(|o| o["id"].clone()).collect();
    let b = run(&mut e, "hwr.prettify_block", json!({"strokes": ids, "force": true}));
    run(&mut e, "hwr.revert_block", json!({"block": b["block"]}));
    assert_eq!(objects(&e, n), before);
}

/// R-HWR-006: mode setting off/suggest/automatic (default suggest); switching never modifies ink.
#[test]
fn r_hwr_006_mode() {
    let (mut e, n) = engine_with_note();
    replay(&mut e, &fixture("loops-4.jsonl"));
    assert_eq!(e.inspect(Some("/settings/prettify_mode")).unwrap(), json!("suggest"));
    let before = objects(&e, n);
    for m in ["off", "automatic", "suggest"] {
        run(&mut e, "hwr.set_mode", json!({"mode": m}));
        assert_eq!(objects(&e, n), before);
    }
}

/// R-HWR-007: a language pack loaded from a directory is listed and used.
#[test]
fn r_hwr_007_language_packs() {
    let (mut e, _) = engine_with_note();
    let r = run(&mut e, "hwr.packs", json!({"dirs": [eval_dir().join("packs")]}));
    assert!(r["packs"].as_array().unwrap().iter().any(|p| p["lang"] == "xx"));
}

/// R-HWR-008: recognition runs in the background and is cancellable; inking is never blocked.
#[test]
fn r_hwr_008_background() {
    let (mut e, n) = engine_with_note();
    replay(&mut e, &fixture("loops-4.jsonl"));
    let ids: Vec<_> = objects(&e, n).iter().map(|o| o["id"].clone()).collect();
    let job = run(
        &mut e,
        "hwr.recognize_block",
        json!({"strokes": ids, "background": true}),
    );
    let t = std::time::Instant::now();
    run(&mut e, "input.pen", json!({"events": fixture("lines-5.jsonl").events}));
    assert!(t.elapsed().as_millis() < 50);
    run(&mut e, "jobs.cancel", json!({"id": job["job"]}));
}
