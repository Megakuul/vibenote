#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Lasso and selection editing (M4.5, R-PEN-009) and barrel buttons (R-PEN-010).

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{NoteId, ObjectKind};

fn setup() -> (Engine, u64) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "s"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    (e, n)
}

fn line(e: &mut Engine, a: (f64, f64), b: (f64, f64)) -> u64 {
    e.execute(
        "ink.stroke.add",
        json!({"samples": [{"x": a.0, "y": a.1, "p": 0.5}, {"x": b.0, "y": b.1, "p": 0.5}]}),
    )
    .unwrap()["id"]
        .as_u64()
        .unwrap()
}

const SQUARE: &str = "[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]";

#[test]
fn half_inside_is_selected_less_is_not() {
    let (mut e, _) = setup();
    let half = line(&mut e, (50.0, 50.0), (150.0, 50.0));
    let less = line(&mut e, (70.0, 60.0), (170.0, 60.0));
    let r = e
        .execute(
            "selection.lasso",
            json!({"path": serde_json::from_str::<Value>(SQUARE).unwrap()}),
        )
        .unwrap();
    assert_eq!(r["ids"], json!([half]));
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), json!([half]));
    let _ = less;
}

#[test]
fn recolour_and_delete_are_one_undo_step_each() {
    let (mut e, n) = setup();
    let a = line(&mut e, (10.0, 10.0), (20.0, 10.0));
    let b = line(&mut e, (10.0, 20.0), (20.0, 20.0));
    e.execute("selection.set", json!({"ids": [b, a]})).unwrap();
    let before = e.session.ws.clone();
    e.execute("selection.recolor", json!({"color": 0xd03030ff_u32}))
        .unwrap();
    for o in e.session.ws.canvas(NoteId(n)).unwrap().objects.values() {
        let ObjectKind::Stroke(s) = &o.kind else { panic!() };
        assert_eq!(s.color.0, 0xd03030ff);
    }
    e.execute("selection.delete", json!({})).unwrap();
    assert!(e.session.ws.canvas(NoteId(n)).unwrap().objects.is_empty());
    e.execute("edit.undo", json!({})).unwrap();
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(e.session.ws, before);
    assert!(e.execute("selection.set", json!({"ids": [999]})).is_err());
}

#[test]
fn hostile_selection_params_are_rejected() {
    let (mut e, _) = setup();
    let a = line(&mut e, (10.0, 10.0), (20.0, 10.0));
    e.execute("selection.set", json!({"ids": [a]})).unwrap();
    let before = e.session.ws.clone();
    for p in [
        json!({"scale": 0.0}),
        json!({"scale": 1e9}),
        json!({"translate": [1e300, 0.0]}),
        json!({"rotate_deg": 1e300}),
        json!({"invert_last": true}),
    ] {
        assert!(e.execute("selection.transform", p.clone()).is_err(), "{p}");
        assert_eq!(e.session.ws, before);
    }
    assert!(
        e.execute("selection.lasso", json!({"path": [[0.0, 0.0], [1.0, 1.0]]}))
            .is_err()
    );
    e.execute("selection.clear", json!({})).unwrap();
    assert!(
        e.execute("selection.transform", json!({"translate": [1.0, 1.0]}))
            .is_err(),
        "nothing selected"
    );
}

fn pen(e: &mut Engine, button: Option<u32>, pts: &[(f64, f64)]) -> Value {
    let mut ev = vec![json!({"t_us": 0, "kind": "proximity_in", "tool": "pen", "x": pts[0].0, "y": pts[0].1})];
    if let Some(b) = button {
        ev.push(json!({"t_us": 1, "kind": "button_down", "tool": "pen", "x": pts[0].0, "y": pts[0].1, "button": b}));
    }
    for (i, &(x, y)) in pts.iter().enumerate() {
        let kind = if i == 0 { "down" } else { "motion" };
        ev.push(json!({"t_us": 10 + i as u64, "kind": kind, "tool": "pen", "x": x, "y": y, "p": 0.5}));
    }
    let last = pts[pts.len() - 1];
    ev.push(json!({"t_us": 1000, "kind": "up", "tool": "pen", "x": last.0, "y": last.1}));
    if let Some(b) = button {
        ev.push(json!({"t_us": 1001, "kind": "button_up", "tool": "pen", "x": last.0, "y": last.1, "button": b}));
    }
    ev.push(json!({"t_us": 1002, "kind": "proximity_out", "tool": "pen", "x": last.0, "y": last.1}));
    e.execute("input.pen", json!({"events": ev})).unwrap()
}

#[test]
fn barrel_buttons_lasso_and_erase_and_the_lasso_tool_selects() {
    let (mut e, n) = setup();
    let a = line(&mut e, (10.0, 10.0), (40.0, 10.0));
    let b = line(&mut e, (10.0, 60.0), (40.0, 60.0));
    // button 1: lasso around `a`, no ink
    let r = pen(&mut e, Some(331), &[(0.0, 0.0), (50.0, 0.0), (50.0, 30.0), (0.0, 30.0)]);
    assert!(r["committed"].as_array().unwrap().is_empty());
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), json!([a]));
    // button 2: erase across `b`
    pen(&mut e, Some(332), &[(25.0, 50.0), (25.0, 70.0)]);
    assert!(
        !e.session
            .ws
            .canvas(NoteId(n))
            .unwrap()
            .objects
            .contains_key(&vibenote_model::ObjectId(b))
    );
    // without a button the pen inks again
    let r = pen(&mut e, None, &[(70.0, 70.0), (90.0, 90.0)]);
    assert_eq!(r["committed"].as_array().unwrap().len(), 1);
    // the lasso tool selects with the tip
    e.execute("tool.select.lasso", json!({})).unwrap();
    let r = pen(
        &mut e,
        None,
        &[(60.0, 60.0), (100.0, 60.0), (100.0, 100.0), (60.0, 100.0)],
    );
    assert!(
        r["committed"].as_array().unwrap().is_empty() && r["errors"].as_array().unwrap().is_empty(),
        "{r}"
    );
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap().as_array().unwrap().len(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    /// Transform then `invert_last` restores the selection exactly (and one undo of each step
    /// does too); the transformed geometry is the affine map within the model's 1/1024 grid.
    #[test]
    fn transform_then_inverse_restores_exactly(
        strokes in prop::collection::vec(prop::collection::vec((-500.0f64..500.0, -500.0f64..500.0), 1..12), 1..6),
        t in (-1e4f64..1e4, -1e4f64..1e4, 0.05f64..20.0, -720.0f64..720.0),
    ) {
        let (mut e, n) = setup();
        let mut ids = vec![];
        for s in &strokes {
            let samples: Vec<Value> = s.iter().map(|&(x, y)| json!({"x": x, "y": y, "p": 0.5})).collect();
            ids.push(e.execute("ink.stroke.add", json!({"samples": samples})).unwrap()["id"].as_u64().unwrap());
        }
        e.execute("selection.set", json!({"ids": ids})).unwrap();
        let before = e.session.ws.clone();
        e.execute("selection.transform", json!({"translate": [t.0, t.1], "scale": t.2, "rotate_deg": t.3, "pivot": [0.0, 0.0]})).unwrap();
        // geometry: each sample moved by the affine map (within the storage grid)
        let (sin, cos) = t.3.to_radians().sin_cos();
        let canvas = e.session.ws.canvas(NoteId(n)).unwrap();
        for (o0, o1) in before.canvas(NoteId(n)).unwrap().objects.values().zip(canvas.objects.values()) {
            let (ObjectKind::Stroke(a), ObjectKind::Stroke(b)) = (&o0.kind, &o1.kind) else { panic!() };
            for (p, q) in a.samples.iter().zip(&b.samples) {
                let (x, y) = (p.x * t.2, p.y * t.2);
                prop_assert!((q.x - (t.0 + x * cos - y * sin)).abs() <= 1e-3);
                prop_assert!((q.y - (t.1 + x * sin + y * cos)).abs() <= 1e-3);
            }
        }
        e.execute("selection.transform", json!({"invert_last": true})).unwrap();
        prop_assert_eq!(&e.session.ws, &before);
        e.execute("edit.undo", json!({})).unwrap();
        e.execute("edit.undo", json!({})).unwrap();
        prop_assert_eq!(&e.session.ws, &before);
    }
}

/// Review M4 L3: ids stored but not loaded yet can be selected on a note still loading.
#[test]
fn selecting_unloaded_objects_of_a_loading_note_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "s"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let far = e
        .execute("ink.stroke.add", json!({"note": n, "samples": [{"x": 1.0e6, "y": 0.0, "p": 0.5}, {"x": 1.0e6 + 9.0, "y": 0.0, "p": 0.5, "t_us": 1}]}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": n, "viewport": [100.0, 100.0]}))
        .unwrap();
    e.execute("selection.set", json!({"ids": [far]})).unwrap();
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), json!([far]));
    assert!(e.execute("selection.set", json!({"ids": [far + 1000]})).is_err());
}
