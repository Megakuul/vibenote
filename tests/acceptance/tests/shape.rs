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

/// R-PEN-007 (owner 2026-10-19): shapes erase like ink — the stroke eraser removes a snapped
/// line; the area eraser cuts a rectangle's outline into ink; undo brings the shapes back.
#[test]
fn r_pen_007_eraser_takes_shapes() {
    let (mut e, n) = engine_with_note();
    let rough = line(&mut e, n, (0.0, 0.0), (100.0, 2.0), 0.5);
    let r = run(&mut e, "shape.snap", json!({"stroke": rough, "held_ms": 600}));
    assert_eq!(r["kind"], "line");
    let r = run(
        &mut e,
        "ink.erase_stroke",
        json!({"path": [[50.0, -10.0], [50.0, 10.0]]}),
    );
    assert_eq!(r["erased"].as_array().unwrap().len(), 1, "{r}");
    assert!(objects(&e, n).is_empty());
    run(&mut e, "edit.undo", json!({}));
    assert_eq!(objects(&e, n)[0]["kind"], "shape");

    let (mut e, n) = engine_with_note();
    run(
        &mut e,
        "shape.create",
        json!({"kind": "rectangle", "from": [0.0, 0.0], "to": [200.0, 100.0], "width": 3.0}),
    );
    // Across the middle of the top side.
    let r = run(
        &mut e,
        "ink.erase_area",
        json!({"path": [[100.0, -20.0], [100.0, 20.0]], "radius": 5.0}),
    );
    assert_eq!(r["erased"].as_array().unwrap().len(), 1, "{r}");
    let o = objects(&e, n);
    assert!(o.iter().all(|x| x["kind"] == "stroke"), "{o:?}");
    assert_eq!(o.len(), 2, "the outline cut once: two pieces");
    for x in &o {
        assert_eq!(x["width"], 3.0);
        for p in x["samples"].as_array().unwrap() {
            let (px, py) = (p["x"].as_f64().unwrap(), p["y"].as_f64().unwrap());
            assert!(
                !(py.abs() < 1.0 && (px - 100.0).abs() < 5.0 - 0.5),
                "ink left under the eraser at {px},{py}"
            );
        }
    }
    // Missing the outline (inside the box): nothing happens.
    let r = run(
        &mut e,
        "ink.erase_area",
        json!({"path": [[100.0, 50.0], [110.0, 50.0]]}),
    );
    assert!(r["erased"].as_array().unwrap().is_empty());
    run(&mut e, "edit.undo", json!({}));
    let o = objects(&e, n);
    assert_eq!(o.len(), 1);
    assert_eq!(o[0]["shape"], "rectangle");
}

/// R-SHAPE-002 (owner 2026-10-19): an arrow drawn in one stroke, head first, snaps to an arrow
/// with its tip where the head was.
#[test]
fn r_shape_002_arrows() {
    let (mut e, n) = engine_with_note();
    let mut pts = vec![(370.0, 82.0), (400.0, 100.0), (370.0, 118.0), (400.0, 100.0)];
    pts.extend((1..=20).map(|i| (400.0 - 15.0 * f64::from(i), 100.0 + f64::from(i % 2))));
    let samples: Vec<_> = pts
        .iter()
        .enumerate()
        .map(|(i, (x, y))| json!({"x": x, "y": y, "p": 0.5, "t_us": i as u64 * 1000}))
        .collect();
    let s = id(&run(&mut e, "ink.stroke.add", json!({ "samples": samples })));
    let r = run(&mut e, "shape.snap", json!({"stroke": s, "held_ms": 600}));
    assert_eq!(r["kind"], "arrow");
    let o = objects(&e, n);
    let p = o[0]["points"].as_array().unwrap();
    assert!(
        (p[1]["x"].as_f64().unwrap() - 400.0).abs() < 6.0,
        "tip at the head: {p:?}"
    );
}

/// R-SHAPE-003: the hand-drawn look stays near the clean outline, is not straight, persists and
/// does not change when the shape moves.
#[test]
fn r_shape_003_hand_drawn_look() {
    let (mut e, _) = engine_with_note();
    let r = id(&run(
        &mut e,
        "shape.create",
        json!({"kind": "rectangle", "from": [0.0, 0.0], "to": [200.0, 100.0]}),
    ));
    let drawn = |e: &vibenote_engine::Engine| {
        let (_, v) = e.session.extras_in_view((-50.0, -50.0), 1.0, (400.0, 300.0));
        v
    };
    assert_eq!(drawn(&e).len(), 1, "clean: one outline");
    let st = run(&mut e, "settings.set", json!({"shape_style": "hand_drawn"}));
    assert_eq!(st["shape_style"], "hand_drawn");
    let v = drawn(&e);
    assert_eq!(v.len(), 8, "each side twice");
    let mut most = 0.0f64;
    for st in &v {
        for p in &st.samples {
            let d =
                p.y.abs()
                    .min((p.y - 100.0).abs())
                    .min(p.x.abs())
                    .min((p.x - 200.0).abs());
            assert!(d < 8.0, "{p:?}");
            most = most.max(d);
        }
    }
    assert!(most > 0.5, "not ruler-straight: {most}");
    // Moving it keeps the look.
    run(&mut e, "selection.set", json!({"ids": [r]}));
    run(&mut e, "selection.transform", json!({"translate": [30.0, 0.0]}));
    let w = drawn(&e);
    for (a, b) in v.iter().zip(&w) {
        for (p, q) in a.samples.iter().zip(&b.samples) {
            assert!((p.x + 30.0 - q.x).abs() < 0.01 && (p.y - q.y).abs() < 0.01);
        }
    }
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

/// R-RULER-001: the ruler blocks ink: a stroke crossing it stops at the edge it came from; one
/// beside its end is unchanged.
#[test]
fn r_ruler_001_blocks_ink() {
    let (mut e, n) = engine_with_note();
    run(
        &mut e,
        "ruler.toggle",
        json!({"visible": true, "at": [0.0, 100.0], "angle_deg": 0.0}),
    );
    let ptr = |e: &mut vibenote_engine::Engine, t: u64, kind: &str, x: f64, y: f64| {
        run(
            e,
            "input.pointer",
            json!({"events": [{"t_us": t, "kind": kind, "x": x, "y": y}]}),
        );
    };
    // From above, down through the body and beyond: held on the edge (y = 100).
    ptr(&mut e, 0, "down", 50.0, 20.0);
    ptr(&mut e, 10_000, "motion", 60.0, 90.0);
    ptr(&mut e, 20_000, "motion", 70.0, 140.0);
    ptr(&mut e, 30_000, "motion", 80.0, 260.0);
    let live = e.session.live_preview().unwrap();
    assert!(
        live.samples.iter().all(|s| s.y <= 100.0 + 1e-9),
        "blocked while drawing"
    );
    ptr(&mut e, 40_000, "up", 80.0, 260.0);
    let o = objects(&e, n);
    let ys: Vec<f64> = o[0]["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["y"].as_f64().unwrap())
        .collect();
    assert!(ys.iter().all(|y| *y <= 100.0 + 1e-9), "{ys:?}");
    assert!((ys.last().unwrap() - 100.0).abs() < 1e-9, "stops at the edge: {ys:?}");
    // From below the body, up through it: held on the back edge (y = 172).
    ptr(&mut e, 100_000, "down", 50.0, 300.0);
    ptr(&mut e, 110_000, "motion", 50.0, 120.0);
    ptr(&mut e, 120_000, "motion", 50.0, 20.0);
    ptr(&mut e, 130_000, "up", 50.0, 20.0);
    let o = objects(&e, n);
    for p in o[1]["samples"].as_array().unwrap() {
        assert!(p["y"].as_f64().unwrap() >= 172.0 - 1e-9, "{p}");
    }
    // Beside the ruler's end (it is 1800 long, centred on x = 0): unchanged.
    ptr(&mut e, 200_000, "down", 1000.0, 20.0);
    ptr(&mut e, 210_000, "motion", 1000.0, 260.0);
    ptr(&mut e, 220_000, "up", 1000.0, 260.0);
    let o = objects(&e, n);
    let last = o[2]["samples"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["y"], json!(260.0));
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
