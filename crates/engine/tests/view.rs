#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! View commands (M4.7, R-CANVAS-002).

use proptest::prelude::*;
use serde_json::json;
use vibenote_engine::Engine;

fn engine_with_strokes(pts: &[(f64, f64)]) -> (Engine, u64) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "v"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    for &(x, y) in pts {
        e.execute(
            "ink.stroke.add",
            json!({"samples": [{"x": x, "y": y, "p": 0.5}, {"x": x + 10.0, "y": y + 5.0, "p": 0.5}]}),
        )
        .unwrap();
    }
    (e, n)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    /// Any zoom keeps the anchor's canvas point under the anchor (< 0.5 px) and stays in range.
    #[test]
    fn zoom_is_anchored_and_bounded(steps in prop::collection::vec((0.001f64..1000.0, 0.0f64..4000.0, 0.0f64..3000.0, -500.0f64..500.0), 1..30)) {
        let (mut e, _) = engine_with_strokes(&[]);
        for (f, ax, ay, pan) in steps {
            let before = e.session.view.to_canvas(ax, ay);
            e.execute("view.zoom", json!({"factor": f, "anchor": [ax, ay]})).unwrap();
            let v = e.session.view;
            prop_assert!((0.01..=64.0).contains(&v.zoom));
            let after = v.to_canvas(ax, ay);
            prop_assert!((before.0 - after.0).abs() * v.zoom < 0.5 && (before.1 - after.1).abs() * v.zoom < 0.5);
            let p0 = e.session.view.to_canvas(0.0, 0.0);
            e.execute("view.pan", json!({"dx": pan, "dy": -pan})).unwrap();
            let p1 = e.session.view.to_canvas(pan, -pan);
            prop_assert!((p0.0 - p1.0).abs() * v.zoom < 1e-6 && (p0.1 - p1.1).abs() * v.zoom < 1e-6, "pan moves content with the cursor");
        }
    }
}

#[test]
fn zoom_fit_frames_the_content() {
    let (mut e, _) = engine_with_strokes(&[(1000.0, 500.0), (3000.0, 1500.0)]);
    let v = e.execute("view.zoom_fit", json!({"viewport": [800.0, 600.0]})).unwrap();
    let zoom = v["zoom"].as_f64().unwrap();
    let view = e.session.view;
    // the content's corners are on screen, with a margin
    for (x, y) in [(999.0, 499.0), (3011.0, 1506.0)] {
        let sx = (x - view.origin_x) * zoom;
        let sy = (y - view.origin_y) * zoom;
        assert!(
            (0.0..=800.0).contains(&sx) && (0.0..=600.0).contains(&sy),
            "{x},{y} → {sx},{sy}"
        );
    }
    assert!(zoom > 0.3 && zoom < 0.4, "{zoom}");
    // empty note: back to 100 % at the origin
    let (mut e, _) = engine_with_strokes(&[]);
    e.execute("view.zoom", json!({"factor": 7.0})).unwrap();
    e.execute("view.zoom_fit", json!({})).unwrap();
    assert_eq!((e.session.view.origin_x, e.session.view.zoom), (0.0, 1.0));
}

/// A note still loading is fitted to all of its content (the stored part from the R-tree).
#[test]
fn zoom_fit_counts_content_not_loaded_yet() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "v"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    for x in [0.0, 50_000.0] {
        e.execute(
            "ink.stroke.add",
            json!({"note": n, "samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 1.0, "y": 1.0, "p": 0.5}]}),
        )
        .unwrap();
    }
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": n, "viewport": [100.0, 100.0]}))
        .unwrap();
    let v = e
        .execute("view.zoom_fit", json!({"viewport": [1000.0, 500.0]}))
        .unwrap();
    assert!(v["zoom"].as_f64().unwrap() < 0.02, "fits 50,000 units: {v}");
}

#[test]
fn hostile_view_params_are_rejected() {
    let (mut e, _) = engine_with_strokes(&[]);
    let before = e.session.view;
    for (cmd, p) in [
        ("view.zoom", json!({"factor": 0.0})),
        ("view.zoom", json!({"factor": -2.0})),
        ("view.zoom", json!({"factor": 2.0, "anchor": [1e300, 0.0]})),
        ("view.pan", json!({"dx": 1e9, "dy": 0.0})),
        ("view.zoom_fit", json!({"viewport": [0.0, 10.0]})),
    ] {
        assert!(e.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
        assert_eq!(e.session.view, before);
    }
}
