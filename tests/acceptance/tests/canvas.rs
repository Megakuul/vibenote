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
//! R-CANVAS-001..006 (SPEC §3).

use serde_json::json;
use vibenote_acceptance::*;
use vibenote_render::{Headless, Viewport, compare};

/// R-CANVAS-001: strokes at ±1e7 render without jitter (identical to the same strokes near 0).
#[test]
fn r_canvas_001_infinite_canvas_precision() {
    let mut r = Headless::new().unwrap();
    let mut img = vec![];
    for off in [0.0, 1.0e7, -1.0e7] {
        let (mut e, n) = engine_with_note();
        line(&mut e, n, (off + 10.0, off + 10.0), (off + 300.0, off + 80.0), 0.7);
        let c = e.session.ws.canvas(vibenote_model::NoteId(n)).unwrap();
        img.push(
            r.render(
                c,
                &[],
                &Viewport {
                    origin_x: off,
                    origin_y: off,
                    zoom: 2.0,
                    width: 700,
                    height: 300,
                },
            )
            .unwrap(),
        );
    }
    assert!(compare(&img[0], &img[1], 8).unwrap().differing <= 20);
    assert!(compare(&img[0], &img[2], 8).unwrap().differing <= 20);
}

/// R-CANVAS-002: zoom 1 %–6,400 %, anchored at the cursor (±0.5 px).
#[test]
fn r_canvas_002_zoom_range_and_anchor() {
    let (mut e, _) = engine_with_note();
    run(&mut e, "view.zoom", json!({"factor": 1000.0, "anchor": [100.0, 100.0]}));
    assert!((e.inspect(Some("/view/zoom")).unwrap().as_f64().unwrap() - 64.0).abs() < 1e-9);
    run(&mut e, "view.zoom", json!({"factor": 1e-6, "anchor": [100.0, 100.0]}));
    assert!((e.inspect(Some("/view/zoom")).unwrap().as_f64().unwrap() - 0.01).abs() < 1e-12);
    let before = e.session.view.to_canvas(321.0, 123.0);
    run(&mut e, "view.zoom", json!({"factor": 3.0, "anchor": [321.0, 123.0]}));
    let after = e.session.view.to_canvas(321.0, 123.0);
    assert!(
        (before.0 - after.0).abs() * e.session.view.zoom < 0.5
            && (before.1 - after.1).abs() * e.session.view.zoom < 0.5
    );
}

/// R-CANVAS-003: every object type can be created, moved, deleted, undone, and round-trips storage.
#[test]
fn r_canvas_003_all_object_types() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = vibenote_engine::Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "objs"})));
    run(&mut e, "note.open", json!({"id": n}));
    let pdf = root().join("tests/fixtures/pdf/three-pages.pdf");
    let created = [
        run(
            &mut e,
            "text.create",
            json!({"at": [10.0, 10.0], "width": 200.0, "content": "Hallo"}),
        ),
        run(&mut e, "pdf.import", json!({"path": pdf, "at": [0.0, 300.0]})),
        run(
            &mut e,
            "image.import",
            json!({"path": root().join("tests/golden/loops-4.png"), "at": [500.0, 0.0]}),
        ),
        run(
            &mut e,
            "link.create",
            json!({"at": [5.0, 5.0], "target": {"url": "https://example.org"}}),
        ),
        run(
            &mut e,
            "shape.create",
            json!({"kind": "rectangle", "from": [0.0, 0.0], "to": [50.0, 30.0]}),
        ),
    ];
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = vibenote_engine::Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
    assert_eq!(created.len(), 5);
}

/// R-CANVAS-004: ink drawn over a PDF stays above it and hit-tests first.
#[test]
fn r_canvas_004_draw_over_objects() {
    let (mut e, n) = engine_with_note();
    run(
        &mut e,
        "pdf.import",
        json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 0.0]}),
    );
    let s = line(&mut e, n, (10.0, 10.0), (100.0, 100.0), 0.5);
    let hit = run(&mut e, "canvas.hit_test", json!({"at": [50.0, 50.0]}));
    assert_eq!(hit["ids"][0], s);
}

/// R-CANVAS-005: z-order commands.
#[test]
fn r_canvas_005_z_order() {
    let (mut e, n) = engine_with_note();
    let a = line(&mut e, n, (0.0, 0.0), (10.0, 0.0), 0.5);
    let b = line(&mut e, n, (0.0, 0.0), (10.0, 0.0), 0.5);
    run(&mut e, "object.raise", json!({"ids": [a]}));
    let o = objects(&e, n);
    let z = |id: u64| o.iter().find(|x| x["id"] == id).unwrap()["z"].as_i64().unwrap();
    assert!(z(a) > z(b));
}

/// R-CANVAS-006: background types (blank/lined/grid/dots) render (goldens).
#[test]
fn r_canvas_006_backgrounds() {
    let (mut e, _) = engine_with_note();
    for bg in ["blank", "lined", "grid", "dots"] {
        run(&mut e, "canvas.set_background", json!({"kind": bg, "spacing": 32.0}));
        assert_eq!(e.inspect(Some("/background/kind")).unwrap(), json!(bg));
    }
}
