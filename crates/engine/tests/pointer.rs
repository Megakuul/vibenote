#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Mouse input (M4.6, R-PEN-012): the left button draws with the selected tool at pressure
//! 0.5, the middle button pans, hostile positions are rejected without side effects.

use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{NoteId, ObjectKind, Tool};

fn open() -> (tempfile::TempDir, Engine, u64) {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "m"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    (dir, e, n)
}

fn ev(kind: &str, x: f64, y: f64, t: u64, button: Option<u32>) -> Value {
    let mut v = json!({"t_us": t, "kind": kind, "x": x, "y": y});
    if let Some(b) = button {
        v["button"] = json!(b);
    }
    v
}

fn drag(e: &mut Engine, button: Option<u32>, from: (f64, f64), to: (f64, f64)) -> Value {
    e.execute(
        "input.pointer",
        json!({"events": [
            ev("down", from.0, from.1, 0, button),
            ev("motion", (from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0, 8000, None),
            ev("motion", to.0, to.1, 16000, None),
            ev("up", to.0, to.1, 24000, button),
        ]}),
    )
    .unwrap()
}

fn strokes(e: &Engine, n: u64) -> Vec<vibenote_model::Stroke> {
    e.session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .values()
        .filter_map(|o| match &o.kind {
            ObjectKind::Stroke(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_left_button_draws_at_half_pressure_with_the_selected_tool() {
    let (_d, mut e, n) = open();
    let r = drag(&mut e, None, (10.0, 10.0), (50.0, 20.0));
    assert_eq!(r["committed"].as_array().unwrap().len(), 1, "{r}");
    e.execute("tool.select.highlighter", json!({})).unwrap();
    drag(&mut e, Some(0x110), (10.0, 100.0), (50.0, 100.0));
    let s = strokes(&e, n);
    assert_eq!(s.len(), 2);
    assert!(s.iter().all(|s| s.samples.len() == 3));
    assert!(
        s.iter()
            .all(|s| s.samples.iter().all(|p| (p.pressure - 0.5).abs() < 1e-4))
    );
    let mut tools: Vec<Tool> = s.iter().map(|s| s.tool).collect();
    tools.sort_by_key(|t| format!("{t:?}"));
    assert_eq!(tools, vec![Tool::Highlighter, Tool::Pen]);
    assert!(e.session.live.stroke.is_none());
}

#[test]
fn the_middle_button_pans_and_draws_nothing() {
    let (_d, mut e, n) = open();
    e.execute("view.zoom", json!({"factor": 2.0})).unwrap();
    let before = e.inspect(Some("/view")).unwrap();
    drag(&mut e, Some(0x112), (100.0, 100.0), (140.0, 70.0));
    let after = e.inspect(Some("/view")).unwrap();
    assert!(strokes(&e, n).is_empty());
    // The content follows the cursor: +40 px right, -30 px up at zoom 2.
    let dx = after["origin_x"].as_f64().unwrap() - before["origin_x"].as_f64().unwrap();
    let dy = after["origin_y"].as_f64().unwrap() - before["origin_y"].as_f64().unwrap();
    assert!((dx + 20.0).abs() < 1e-9 && (dy - 15.0).abs() < 1e-9, "{dx} {dy}");
    // After release, motion hovers again and does not pan.
    e.execute(
        "input.pointer",
        json!({"events": [ev("motion", 0.0, 0.0, 30000, None)]}),
    )
    .unwrap();
    assert_eq!(e.inspect(Some("/view")).unwrap(), after);
}

#[test]
fn the_mouse_lassoes_and_erases_with_those_tools() {
    let (_d, mut e, n) = open();
    drag(&mut e, None, (10.0, 10.0), (50.0, 10.0));
    let id = e
        .session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .keys()
        .next()
        .copied()
        .unwrap();
    e.execute("tool.select.lasso", json!({})).unwrap();
    let r = e
        .execute(
            "input.pointer",
            json!({"events": [ev("down", 0.0, 0.0, 0, None), ev("motion", 100.0, 0.0, 1, None), ev("motion", 100.0, 40.0, 2, None), ev("motion", 0.0, 40.0, 3, None), ev("up", 0.0, 40.0, 4, None)]}),
        )
        .unwrap();
    assert!(r["errors"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), json!([id.0]));
    e.execute("tool.select.eraser_stroke", json!({})).unwrap();
    drag(&mut e, None, (30.0, 0.0), (30.0, 20.0));
    assert!(strokes(&e, n).is_empty());
}

#[test]
fn other_buttons_are_ignored_and_hostile_positions_rejected() {
    let (_d, mut e, n) = open();
    drag(&mut e, Some(0x111), (10.0, 10.0), (50.0, 10.0));
    assert!(strokes(&e, n).is_empty());
    let before = e.inspect(None).unwrap();
    for bad in [f64::NAN, f64::INFINITY, 1.0e9] {
        let r = e.execute(
            "input.pointer",
            json!({"events": [ev("down", 1.0, 1.0, 0, None), {"t_us": 1, "kind": "motion", "x": bad, "y": 1.0}]}),
        );
        assert!(r.is_err(), "{bad}");
    }
    assert!(
        e.execute(
            "input.pointer",
            json!({"events": [{"t_us": 0, "kind": "wheel", "x": 0.0, "y": 0.0}]})
        )
        .is_err()
    );
    assert_eq!(e.inspect(None).unwrap(), before, "a rejected batch changes nothing");
}

#[test]
fn the_lasso_and_the_selection_show_as_an_overlay() {
    let (_d, mut e, n) = open();
    drag(&mut e, None, (10.0, 10.0), (50.0, 10.0));
    // After inking only the pen footprint shows where the mouse is (R-PEN-011): one dot at
    // the release point, no lasso or selection.
    let o = e.session.overlay();
    assert_eq!(o.len(), 1, "{o:?}");
    let at: Vec<(f64, f64)> = o[0].samples.iter().map(|s| (s.x, s.y)).collect();
    assert_eq!(at, vec![(50.0, 10.0), (50.0, 10.0)]);
    e.execute("tool.select.lasso", json!({})).unwrap();
    e.execute("view.zoom", json!({"factor": 2.0})).unwrap();
    // Mid-drag: the lasso path, closed, 1.5 px wide on screen; nothing is inked.
    e.execute(
        "input.pointer",
        json!({"events": [ev("down", 0.0, 0.0, 0, None), ev("motion", 200.0, 0.0, 1, None), ev("motion", 200.0, 80.0, 2, None)]}),
    )
    .unwrap();
    assert!(e.session.live_preview().is_none());
    let o = e.session.overlay();
    assert_eq!(o.len(), 1);
    let pts: Vec<(f64, f64)> = o[0].samples.iter().map(|s| (s.x, s.y)).collect();
    assert_eq!(pts, vec![(0.0, 0.0), (100.0, 0.0), (100.0, 40.0), (0.0, 0.0)]);
    assert!((f64::from(o[0].width) - 0.75).abs() < 1e-6);
    // Released: the stroke is selected and boxed.
    e.execute(
        "input.pointer",
        json!({"events": [ev("motion", 0.0, 80.0, 3, None), ev("up", 0.0, 80.0, 4, None)]}),
    )
    .unwrap();
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap().as_array().unwrap().len(), 1);
    // The box first, then its four corner handles and the rotate stem and knob (R-PEN-009).
    let o = e.session.overlay();
    assert_eq!(o.len(), 7);
    let b = o[0]
        .samples
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), s| (lo.min(s.x), hi.max(s.x)));
    assert!(b.0 < 10.0 && b.1 > 50.0, "box around the stroke: {b:?}");
    // Another note: no box.
    let m = e.execute("workspace.note.create", json!({"name": "m2"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": m})).unwrap();
    assert!(e.session.overlay().is_empty());
    let _ = n;
}

/// Review M4 M4: the mouse and the pen never mix into one stroke.
#[test]
fn mouse_and_pen_keep_separate_strokes() {
    let (_d, mut e, n) = open();
    let pen = |kind: &str, x: f64, t: u64| json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": 10.0, "p": 0.8});
    // Pen writing; a touchpad click arrives mid-stroke: ignored.
    e.execute(
        "input.pen",
        json!({"events": [pen("proximity_in", 0.0, 0), pen("down", 0.0, 1), pen("motion", 10.0, 2)]}),
    )
    .unwrap();
    e.execute("input.pointer", json!({"events": [ev("down", 300.0, 300.0, 3, None), ev("motion", 310.0, 300.0, 4, None), ev("up", 310.0, 300.0, 5, None)]}))
        .unwrap();
    e.execute(
        "input.pen",
        json!({"events": [pen("motion", 20.0, 6), pen("up", 20.0, 7), pen("proximity_out", 20.0, 8)]}),
    )
    .unwrap();
    let s = strokes(&e, n);
    assert_eq!(s.len(), 1);
    assert!(
        s[0].samples
            .iter()
            .all(|p| (p.pressure - 0.8).abs() < 1e-4 && p.y == 10.0)
    );
    assert_eq!(s[0].samples.len(), 3);
    // Mouse drawing; the pen hovers in and out meanwhile: the mouse stroke is unaffected.
    e.execute(
        "input.pointer",
        json!({"events": [ev("down", 0.0, 100.0, 10, None), ev("motion", 10.0, 100.0, 11, None)]}),
    )
    .unwrap();
    e.execute(
        "input.pen",
        json!({"events": [pen("proximity_in", 500.0, 12), pen("motion", 510.0, 13), pen("proximity_out", 510.0, 14)]}),
    )
    .unwrap();
    e.execute(
        "input.pointer",
        json!({"events": [ev("motion", 20.0, 100.0, 15, None), ev("up", 20.0, 100.0, 16, None)]}),
    )
    .unwrap();
    let s = strokes(&e, n);
    assert_eq!(s.len(), 2);
    let m = s.iter().find(|s| s.samples[0].y == 100.0).unwrap();
    assert_eq!(m.samples.len(), 3);
    assert!(m.samples.iter().all(|p| (p.pressure - 0.5).abs() < 1e-4));
}

/// Review M4 M3: the cached selection box follows edits of the selected strokes.
#[test]
fn the_selection_box_follows_transforms() {
    let (_d, mut e, n) = open();
    drag(&mut e, None, (10.0, 10.0), (50.0, 10.0));
    let id = strokes_ids(&e, n)[0];
    e.execute("selection.set", json!({"ids": [id]})).unwrap();
    let minx = |e: &Engine| {
        e.session.overlay()[0]
            .samples
            .iter()
            .map(|s| s.x)
            .fold(f64::MAX, f64::min)
    };
    let before = minx(&e);
    assert_eq!(minx(&e), before, "cached");
    e.execute("selection.transform", json!({"translate": [100.0, 0.0]}))
        .unwrap();
    assert!((minx(&e) - before - 100.0).abs() < 1e-6);
    e.execute("edit.undo", json!({})).unwrap();
    assert!((minx(&e) - before).abs() < 1e-6);
}

fn strokes_ids(e: &Engine, n: u64) -> Vec<u64> {
    e.session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .keys()
        .map(|i| i.0)
        .collect()
}
