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
//! R-SHAPE-001..002, R-RULER-001 (SPEC §8).

use serde_json::json;
use vibenote_acceptance::*;

/// R-SHAPE-001: line, arrow, rectangle, ellipse, polygon.
#[test]
fn r_shape_001_shape_tool() {
    let (mut e, n) = engine_with_note();
    for k in ["line", "arrow", "rectangle", "ellipse", "polygon"] {
        run(
            &mut e,
            "shape.create",
            json!({"kind": k, "from": [0.0, 0.0], "to": [40.0, 30.0], "points": [[0.0, 0.0], [10.0, 0.0], [5.0, 8.0]]}),
        );
    }
    assert_eq!(objects(&e, n).len(), 5);
}

/// R-SHAPE-002: hold-to-snap converts rough shapes (≥ 95 % correct on the fixture set); strokes
/// that are not held are never converted.
#[test]
fn r_shape_002_snap() {
    let (mut e, n) = engine_with_note();
    let rough = line(&mut e, n, (0.0, 0.0), (100.0, 2.0), 0.5);
    let r = run(&mut e, "shape.snap", json!({"stroke": rough, "held_ms": 600}));
    assert_eq!(r["kind"], "line");
    let not_held = line(&mut e, n, (0.0, 50.0), (100.0, 52.0), 0.5);
    let r = run(&mut e, "shape.snap", json!({"stroke": not_held, "held_ms": 100}));
    assert_eq!(r["kind"], serde_json::Value::Null);
}

/// R-RULER-001: strokes starting near the ruler edge snap to it (max deviation 0.5 px).
#[test]
fn r_ruler_001_snapping() {
    let (mut e, n) = engine_with_note();
    run(
        &mut e,
        "ruler.toggle",
        json!({"visible": true, "at": [0.0, 100.0], "angle_deg": 0.0}),
    );
    let s = line(&mut e, n, (5.0, 108.0), (200.0, 103.0), 0.5);
    let o = objects(&e, n);
    let st = o.iter().find(|x| x["id"] == s).unwrap();
    for p in st["samples"].as_array().unwrap() {
        assert!((p["y"].as_f64().unwrap() - 100.0).abs() <= 0.5);
    }
}
