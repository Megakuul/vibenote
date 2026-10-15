#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! Configurable barrel buttons (R-PEN-010; review M4 L1), pen-barrel pan (R-CANVAS-002) and the
//! eraser overlay (review M4 M6).

use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::NoteId;

fn ev(kind: &str, x: f64, t: u64, button: Option<u32>) -> Value {
    let mut v = json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": 10.0, "p": 0.6});
    if let Some(b) = button {
        v["button"] = json!(b);
    }
    v
}

/// One pen stroke from x0 to x1 with `button` held (if any).
fn stroke(e: &mut Engine, button: Option<u32>, x0: f64, x1: f64) -> Value {
    let mut evs = vec![ev("proximity_in", x0, 0, None)];
    if let Some(b) = button {
        evs.push(ev("button_down", x0, 1, Some(b)));
    }
    evs.extend([
        ev("down", x0, 2, None),
        ev("motion", (x0 + x1) / 2.0, 3, None),
        ev("motion", x1, 4, None),
        ev("up", x1, 5, None),
    ]);
    if let Some(b) = button {
        evs.push(ev("button_up", x1, 6, Some(b)));
    }
    evs.push(ev("proximity_out", x1, 7, None));
    e.execute("input.pen", json!({ "events": evs })).unwrap()
}

fn setup() -> (Engine, u64) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "b"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    (e, n)
}

fn count(e: &Engine, n: u64) -> usize {
    e.session.ws.canvas(NoteId(n)).unwrap().objects.len()
}

#[test]
fn barrel_actions_follow_the_settings() {
    let (mut e, n) = setup();
    stroke(&mut e, None, 0.0, 100.0);
    assert_eq!(count(&e, n), 1);
    // Defaults: 1 = lasso (selects, no ink), 2 = eraser.
    assert_eq!(e.inspect(Some("/settings/barrel1")).unwrap(), json!("lasso"));
    // Swap them: button 2 now lassoes, button 1 erases.
    e.execute("settings.set", json!({"barrel1": "eraser", "barrel2": "lasso"}))
        .unwrap();
    stroke(&mut e, Some(332), 200.0, 300.0);
    assert_eq!(count(&e, n), 1, "a lasso draws no ink");
    stroke(&mut e, Some(331), 50.0, 50.5);
    assert_eq!(count(&e, n), 0, "button 1 erased the stroke");
    // "none": the pen inks as if no button were held.
    e.execute("settings.set", json!({"barrel1": "none"})).unwrap();
    stroke(&mut e, Some(331), 0.0, 100.0);
    assert_eq!(count(&e, n), 1);
    assert!(e.execute("settings.set", json!({"barrel1": "teleport"})).is_err());
}

#[test]
fn a_pan_barrel_button_drags_the_view_and_draws_nothing() {
    let (mut e, n) = setup();
    e.execute("settings.set", json!({"barrel2": "pan"})).unwrap();
    let before = e.inspect(Some("/view/origin_x")).unwrap().as_f64().unwrap();
    stroke(&mut e, Some(332), 100.0, 160.0);
    let after = e.inspect(Some("/view/origin_x")).unwrap().as_f64().unwrap();
    assert!(
        (before - after - 60.0).abs() < 1e-9,
        "content follows the pen: {before} → {after}"
    );
    assert_eq!(count(&e, n), 0);
    // Afterwards the pen draws normally again.
    stroke(&mut e, None, 0.0, 50.0);
    assert_eq!(count(&e, n), 1);
}

#[test]
fn erasing_shows_the_swept_path() {
    let (mut e, _) = setup();
    e.execute("tool.select.eraser", json!({})).unwrap();
    e.execute(
        "input.pen",
        json!({"events": [ev("proximity_in", 0.0, 0, None), ev("down", 0.0, 1, None), ev("motion", 40.0, 2, None)]}),
    )
    .unwrap();
    let o = e.session.overlay();
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].samples.len(), 2);
    assert!(
        (f64::from(o[0].width) - 16.0).abs() < 1e-6,
        "as wide as the eraser at zoom 1"
    );
    assert!(e.session.live_preview().is_none(), "no ink while erasing");
}
