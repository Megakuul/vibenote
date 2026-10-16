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

/// R-RULER-001: shown and drawn; its body drags it, its knob rotates it; strokes along its edge
/// are straight already while drawing; Alt+Left/Right turn by 15°.
#[test]
fn r_ruler_001_moves_rotates_and_draws_along() {
    let (mut e, n) = engine_with_note();
    run(
        &mut e,
        "ruler.toggle",
        json!({"visible": true, "at": [0.0, 100.0], "angle_deg": 0.0}),
    );
    assert!(!e.session.overlay().is_empty(), "the ruler is drawn");
    let ptr = |e: &mut vibenote_engine::Engine, t: u64, kind: &str, x: f64, y: f64| {
        run(
            e,
            "input.pointer",
            json!({"events": [{"t_us": t, "kind": kind, "x": x, "y": y}]}),
        );
    };
    // Drag the body (40 px into it) down by 50: the ruler moves, nothing is drawn.
    ptr(&mut e, 0, "down", 10.0, 140.0);
    ptr(&mut e, 10_000, "motion", 30.0, 190.0);
    ptr(&mut e, 20_000, "up", 30.0, 190.0);
    let r = e.session.ruler;
    assert!((r.x - 20.0).abs() < 1e-9 && (r.y - 150.0).abs() < 1e-9, "{r:?}");
    assert!(objects(&e, n).is_empty());
    // A stroke started near the edge is laid along it while drawing.
    ptr(&mut e, 100_000, "down", 40.0, 145.0);
    ptr(&mut e, 110_000, "motion", 120.0, 141.0);
    let live = e.session.live_preview().unwrap();
    assert!(
        live.samples.iter().all(|s| (s.y - 150.0).abs() < 1e-6),
        "straight while drawing"
    );
    ptr(&mut e, 120_000, "up", 120.0, 141.0);
    assert_eq!(objects(&e, n).len(), 1);
    // The knob rotates it.
    let (kx, ky) = r.knob(1.0);
    ptr(&mut e, 200_000, "down", kx, ky);
    let (dx, dy) = (kx - r.x, ky - r.y);
    let a = 30f64.to_radians();
    ptr(
        &mut e,
        210_000,
        "motion",
        r.x + dx * a.cos() - dy * a.sin(),
        r.y + dx * a.sin() + dy * a.cos(),
    );
    ptr(&mut e, 220_000, "up", 0.0, 0.0);
    assert!(
        (e.session.ruler.angle_deg - 30.0).abs() < 1e-6,
        "{}",
        e.session.ruler.angle_deg
    );
    run(&mut e, "ruler.rotate_right", json!({}));
    assert!((e.session.ruler.angle_deg - 45.0).abs() < 1e-9);
    run(&mut e, "ruler.rotate_left", json!({}));
    run(&mut e, "ruler.rotate_left", json!({}));
    assert!((e.session.ruler.angle_deg - 15.0).abs() < 1e-9);
}
