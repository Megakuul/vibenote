#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Tools (M4.1, R-PEN-005): selection is a setting; the pen tip uses the selected tool.

use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{NoteId, ObjectKind, Tool};

fn pen(e: &mut Engine, x: f64) -> Value {
    let ev = |kind: &str, x: f64, t: u64| json!({"kind": kind, "tool": "pen", "x": x, "y": 10.0, "p": 0.5, "tx": 30.0, "ty": 0.0, "t_us": t});
    e.execute(
        "input.pen",
        json!({"events": [ev("proximity_in", x, 0), ev("down", x, 1000), ev("motion", x + 20.0, 2000), ev("up", x + 20.0, 3000), ev("proximity_out", x + 20.0, 4000)]}),
    )
    .unwrap()
}

#[test]
fn the_pen_tip_draws_with_the_selected_tool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "t"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    assert_eq!(e.inspect(Some("/tool")).unwrap(), json!("pen"));
    pen(&mut e, 0.0);
    e.execute("tool.select.highlighter", json!({})).unwrap();
    assert_eq!(e.inspect(Some("/tool")).unwrap(), json!("highlighter"));
    assert_eq!(e.inspect(Some("/settings/tool")).unwrap(), json!("highlighter"));
    let r = pen(&mut e, 100.0);
    assert_eq!(r["committed"].as_array().unwrap().len(), 1);
    let tools: Vec<Tool> = e
        .session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .values()
        .filter_map(|o| match &o.kind {
            ObjectKind::Stroke(s) => Some(s.tool),
            _ => None,
        })
        .collect();
    assert_eq!(tools, vec![Tool::Pen, Tool::Highlighter]);
    // The shape tool commits no ink: the drag becomes a shape (R-SHAPE-001).
    e.execute("tool.select.shape", json!({})).unwrap();
    let r = pen(&mut e, 200.0);
    assert!(r["committed"].as_array().unwrap().is_empty());
    assert_eq!(r["errors"], json!([]), "{r}");
    let shapes = e
        .session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .values()
        .filter(|o| matches!(o.kind, ObjectKind::Shape { .. }))
        .count();
    assert_eq!(shapes, 1);
    // The selection persists.
    drop(e);
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.inspect(Some("/tool")).unwrap(), json!("shape"));
}

#[test]
fn the_live_preview_uses_the_selected_tool() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "t"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    e.execute("tool.select.highlighter", json!({})).unwrap();
    e.execute(
        "input.pen",
        json!({"events": [{"kind": "proximity_in", "tool": "pen", "x": 0.0, "y": 0.0, "p": 0.0, "t_us": 0},
                          {"kind": "down", "tool": "pen", "x": 0.0, "y": 0.0, "p": 0.4, "t_us": 10}]}),
    )
    .unwrap();
    assert_eq!(e.session.live_preview().unwrap().tool, Tool::Highlighter);
}
