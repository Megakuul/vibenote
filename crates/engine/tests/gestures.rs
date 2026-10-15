#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Dragging the selection by its box and handles with the pen and the mouse (R-PEN-009), and
//! the cursor hints and footprint the UI shows (R-PEN-011).

use serde_json::{Value, json};
use vibenote_engine::gesture::Corner;
use vibenote_engine::{CursorHint, Engine};
use vibenote_model::{NoteId, ObjectId, ObjectKind};

fn setup() -> (Engine, u64) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "g"})).unwrap()["id"]
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

fn mouse(e: &mut Engine, kind: &str, x: f64, y: f64) -> Value {
    e.execute(
        "input.pointer",
        json!({"events": [{"t_us": 1000, "kind": kind, "x": x, "y": y, "button": 272}]}),
    )
    .unwrap()
}

fn pen(e: &mut Engine, kind: &str, x: f64, y: f64) -> Value {
    e.execute(
        "input.pen",
        json!({"events": [{"t_us": 1000, "kind": kind, "tool": "pen", "x": x, "y": y, "p": 0.5}]}),
    )
    .unwrap()
}

fn first_sample(e: &Engine, n: u64, id: u64) -> (f64, f64) {
    let o = &e.session.ws.canvas(NoteId(n)).unwrap().objects[&ObjectId(id)];
    let ObjectKind::Stroke(s) = &o.kind else { panic!() };
    (s.samples[0].x, s.samples[0].y)
}

fn undo_len(e: &Engine) -> usize {
    e.inspect(Some("/history"))
        .ok()
        .and_then(|h| {
            h.as_object()
                .map(|m| m.values().map(|v| v["undo"].as_array().map_or(0, Vec::len)).sum())
        })
        .unwrap_or(0)
}

/// Two strokes selected; box (100,100)–(200,150) plus the margin.
fn selected(e: &mut Engine) -> (u64, u64) {
    let a = line(e, (100.0, 100.0), (200.0, 100.0));
    let b = line(e, (100.0, 150.0), (200.0, 150.0));
    e.execute("selection.set", json!({"ids": [a, b]})).unwrap();
    (a, b)
}

#[test]
fn dragging_inside_the_box_moves_the_selection_as_one_undo_step() {
    let (mut e, n) = setup();
    let (a, b) = selected(&mut e);
    let before = e.session.ws.clone();
    let steps = undo_len(&e);
    mouse(&mut e, "down", 150.0, 125.0);
    mouse(&mut e, "motion", 170.0, 135.0);
    // Previewed by a display transform; nothing is stored yet.
    assert_eq!(e.session.ws, before);
    let d = e.session.display_transforms(NoteId(n));
    assert_eq!(d.get(&ObjectId(a)), Some(&[1.0, 0.0, -0.0, 1.0, 20.0, 10.0]));
    assert_eq!(e.session.cursor_hint(), CursorHint::Grabbing);
    mouse(&mut e, "motion", 200.0, 145.0);
    let r = mouse(&mut e, "up", 200.0, 145.0);
    assert_eq!(r["errors"], json!([]));
    assert_eq!(first_sample(&e, n, a), (150.0, 120.0));
    assert_eq!(first_sample(&e, n, b), (150.0, 170.0));
    assert!(e.session.display_transforms(NoteId(n)).is_empty());
    assert_eq!(undo_len(&e), steps + 1);
    // Still selected: it can be dragged again.
    assert_eq!(e.session.selection.ids, vec![a, b]);
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(e.session.ws, before);
}

#[test]
fn the_pen_drags_the_selection_too() {
    let (mut e, n) = setup();
    let (a, _) = selected(&mut e);
    pen(&mut e, "down", 150.0, 125.0);
    pen(&mut e, "motion", 150.0, 75.0);
    let r = pen(&mut e, "up", 150.0, 75.0);
    assert_eq!(r["committed"], json!([]), "no ink drawn");
    assert_eq!(first_sample(&e, n, a), (100.0, 50.0));
}

#[test]
fn a_corner_scales_around_the_opposite_corner() {
    let (mut e, n) = setup();
    let (a, b) = selected(&mut e);
    // Bottom-right corner of the box: (200, 150) + 6 px margin.
    e.session.live.hover = Some((206.0, 156.0));
    assert_eq!(e.session.cursor_hint(), CursorHint::Scale(Corner::BottomRight));
    mouse(&mut e, "down", 206.0, 156.0);
    // The corner moves by the box's diagonal: twice the size, the opposite corner stays.
    let r = e.session.selection_rect().unwrap();
    let (w, h) = (r.width(), r.height());
    mouse(&mut e, "motion", 206.0 + w, 156.0 + h);
    mouse(&mut e, "up", 206.0 + w, 156.0 + h);
    let map = |(x, y): (f64, f64)| (r.min.x + 2.0 * (x - r.min.x), r.min.y + 2.0 * (y - r.min.y));
    for (id, at) in [(a, (100.0, 100.0)), (b, (100.0, 150.0))] {
        let (x, y) = first_sample(&e, n, id);
        let want = map(at);
        assert!(
            (x - want.0).abs() < 1e-2 && (y - want.1).abs() < 1e-2,
            "{x},{y} vs {want:?}"
        );
    }
    let o = &e.session.ws.canvas(NoteId(n)).unwrap().objects[&ObjectId(a)];
    let ObjectKind::Stroke(s) = &o.kind else { panic!() };
    assert!((s.width - 4.0).abs() < 1e-3, "widths scale too");
}

#[test]
fn the_knob_rotates_around_the_centre() {
    let (mut e, n) = setup();
    let (a, _) = selected(&mut e);
    // Knob: 24 px above the top edge's middle (150, 94).
    e.session.live.hover = Some((150.0, 70.0));
    assert_eq!(e.session.cursor_hint(), CursorHint::Rotate);
    mouse(&mut e, "down", 150.0, 70.0);
    // A quarter turn clockwise around the centre (150, 125).
    mouse(&mut e, "motion", 205.0, 125.0);
    mouse(&mut e, "up", 205.0, 125.0);
    let (x, y) = first_sample(&e, n, a);
    assert!((x - 175.0).abs() < 1e-2 && (y - 75.0).abs() < 1e-2, "{x},{y}");
}

#[test]
fn a_tap_on_the_selection_changes_nothing() {
    let (mut e, _) = setup();
    selected(&mut e);
    let before = e.session.ws.clone();
    let steps = undo_len(&e);
    mouse(&mut e, "down", 150.0, 125.0);
    mouse(&mut e, "up", 150.0, 125.0);
    assert_eq!(e.session.ws, before);
    assert_eq!(undo_len(&e), steps);
}

#[test]
fn a_press_elsewhere_drops_the_selection_and_draws() {
    let (mut e, _) = setup();
    selected(&mut e);
    pen(&mut e, "down", 400.0, 400.0);
    pen(&mut e, "motion", 420.0, 410.0);
    let r = pen(&mut e, "up", 420.0, 410.0);
    assert_eq!(r["committed"].as_array().unwrap().len(), 1, "{r}");
    assert!(e.session.selection.ids.is_empty());
}

#[test]
fn the_cursor_hint_follows_the_tool_and_the_selection() {
    let (mut e, _) = setup();
    assert_eq!(e.session.cursor_hint(), CursorHint::Ink);
    for (tool, hint) in [
        ("tool.select.lasso", CursorHint::Lasso),
        ("tool.select.shape", CursorHint::Shape),
        ("tool.select.text", CursorHint::Text),
        ("tool.select.eraser", CursorHint::Erase),
        ("tool.select.highlighter", CursorHint::Ink),
    ] {
        e.execute(tool, json!({})).unwrap();
        assert_eq!(e.session.cursor_hint(), hint, "{tool}");
    }
    selected(&mut e);
    mouse(&mut e, "motion", 150.0, 125.0);
    assert_eq!(e.session.cursor_hint(), CursorHint::Move);
    mouse(&mut e, "motion", 400.0, 400.0);
    assert_eq!(e.session.cursor_hint(), CursorHint::Ink);
}

#[test]
fn the_footprint_follows_the_hover_and_goes_with_the_pointer() {
    let (mut e, _) = setup();
    let base = e.session.overlay().len();
    mouse(&mut e, "motion", 50.0, 60.0);
    assert_eq!(e.session.live.hover, Some((50.0, 60.0)));
    let o = e.session.overlay();
    assert_eq!(o.len(), base + 1, "a footprint dot");
    assert_eq!((o[base].samples[0].x, o[base].samples[0].y), (50.0, 60.0));
    mouse(&mut e, "leave", 50.0, 60.0);
    assert_eq!(e.session.live.hover, None);
    assert_eq!(e.session.overlay().len(), base);
}

#[test]
fn lasso_selects_shapes_and_text_by_area() {
    let (mut e, _) = setup();
    let s = e
        .execute(
            "shape.create",
            json!({"kind": "rectangle", "from": [10.0, 10.0], "to": [50.0, 40.0]}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let out = e
        .execute(
            "shape.create",
            json!({"kind": "rectangle", "from": [500.0, 500.0], "to": [600.0, 600.0]}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let r = e
        .execute(
            "selection.lasso",
            json!({"path": [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]}),
        )
        .unwrap();
    assert_eq!(r["ids"], json!([s]));
    let _ = out;
}

fn shapes(e: &Engine, n: u64) -> Vec<(vibenote_model::ShapeKind, usize)> {
    e.session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .values()
        .filter_map(|o| match &o.kind {
            ObjectKind::Shape { shape, points, .. } => Some((*shape, points.len())),
            _ => None,
        })
        .collect()
}

fn strokes(e: &Engine, n: u64) -> usize {
    e.session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .values()
        .filter(|o| matches!(o.kind, ObjectKind::Stroke(_)))
        .count()
}

#[test]
fn the_shape_tool_draws_the_selected_shape_by_dragging() {
    let (mut e, n) = setup();
    e.execute("tool.select.shape", json!({})).unwrap();
    assert_eq!(
        e.session.settings.shape,
        vibenote_model::ShapeKind::Rectangle,
        "default"
    );
    for (kind, want) in [
        ("rectangle", vibenote_model::ShapeKind::Rectangle),
        ("ellipse", vibenote_model::ShapeKind::Ellipse),
        ("line", vibenote_model::ShapeKind::Line),
        ("arrow", vibenote_model::ShapeKind::Arrow),
        ("triangle", vibenote_model::ShapeKind::Triangle),
    ] {
        e.execute("settings.set", json!({"shape": kind})).unwrap();
        pen(&mut e, "down", 10.0, 10.0);
        pen(&mut e, "motion", 60.0, 40.0);
        // Previewed while dragging.
        assert!(!e.session.overlay().is_empty());
        let r = pen(&mut e, "up", 80.0, 60.0);
        assert_eq!(r["errors"], json!([]), "{kind}");
        assert!(shapes(&e, n).iter().any(|(k, _)| *k == want), "{kind}");
    }
    assert_eq!(strokes(&e, n), 0, "the shape tool never inks");
    // A tap makes nothing.
    let before = shapes(&e, n).len();
    pen(&mut e, "down", 300.0, 300.0);
    pen(&mut e, "up", 300.0, 300.0);
    assert_eq!(shapes(&e, n).len(), before);
}

#[test]
fn polygons_are_placed_tap_by_tap_and_closed_on_the_first_corner() {
    let (mut e, n) = setup();
    e.execute("settings.set", json!({"tool": "shape", "shape": "polygon"}))
        .unwrap();
    for (x, y) in [(10.0, 10.0), (100.0, 10.0), (120.0, 80.0), (30.0, 90.0)] {
        pen(&mut e, "down", x, y);
        pen(&mut e, "up", x, y);
    }
    assert_eq!(e.session.live.polygon.len(), 4);
    assert!(shapes(&e, n).is_empty());
    pen(&mut e, "down", 12.0, 11.0);
    pen(&mut e, "up", 12.0, 11.0);
    assert_eq!(shapes(&e, n), vec![(vibenote_model::ShapeKind::Polygon, 4)]);
    assert!(e.session.live.polygon.is_empty());
    // Escape drops a polygon in progress.
    pen(&mut e, "down", 10.0, 10.0);
    pen(&mut e, "up", 10.0, 10.0);
    e.execute("selection.clear", json!({})).unwrap();
    assert!(e.session.live.polygon.is_empty());
}

/// A rough rectangle drawn with the pen tool, timestamps 10 ms apart from `t0`.
fn rough_rectangle(e: &mut Engine, t0: u64) -> u64 {
    let mut pts = Vec::new();
    for i in 0..=20 {
        pts.push((100.0 + 10.0 * f64::from(i), 100.0 + f64::from(i % 2)));
    }
    for i in 0..=10 {
        pts.push((300.0 + f64::from(i % 2), 100.0 + 10.0 * f64::from(i)));
    }
    for i in 0..=20 {
        pts.push((300.0 - 10.0 * f64::from(i), 200.0 + f64::from(i % 2)));
    }
    for i in 0..=10 {
        pts.push((100.0 + f64::from(i % 2), 200.0 - 10.0 * f64::from(i)));
    }
    let mut t = t0;
    let ev = |kind: &str, (x, y): (f64, f64), t: u64| json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": y, "p": 0.5});
    let mut events = vec![ev("down", pts[0], t)];
    for p in &pts[1..] {
        t += 10_000;
        events.push(ev("motion", *p, t));
    }
    e.execute("input.pen", json!({ "events": events })).unwrap();
    t
}

#[test]
fn holding_the_pen_still_snaps_a_rough_shape_and_undo_returns_the_ink() {
    let (mut e, n) = setup();
    let t = rough_rectangle(&mut e, 1_000_000);
    // Not held long enough yet.
    assert!(!e.tick_pen(t + 400_000).unwrap());
    assert_eq!(strokes(&e, n) + shapes(&e, n).len(), 0);
    assert!(e.tick_pen(t + 510_000).unwrap());
    assert_eq!(shapes(&e, n).len(), 1, "snapped");
    assert_eq!(shapes(&e, n)[0].0, vibenote_model::ShapeKind::Rectangle);
    assert_eq!(strokes(&e, n), 0);
    // The rest of the touch draws nothing.
    pen(&mut e, "motion", 500.0, 500.0);
    let r = pen(&mut e, "up", 500.0, 500.0);
    assert_eq!(r["committed"], json!([]));
    assert_eq!(strokes(&e, n), 0);
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!((strokes(&e, n), shapes(&e, n).len()), (1, 0), "back to the ink");
}

#[test]
fn small_or_unheld_strokes_never_snap() {
    let (mut e, n) = setup();
    // A small closed loop (a letter "o"): held, but too small.
    let mut events = vec![json!({"t_us": 0, "kind": "down", "tool": "pen", "x": 10.0, "y": 10.0, "p": 0.5})];
    for i in 1..=24 {
        let a = f64::from(i) / 24.0 * std::f64::consts::TAU;
        events.push(json!({"t_us": i * 10_000, "kind": "motion", "tool": "pen", "x": 10.0 + 6.0 * a.sin(), "y": 16.0 - 6.0 * a.cos(), "p": 0.5}));
    }
    e.execute("input.pen", json!({ "events": events })).unwrap();
    assert!(!e.tick_pen(10_000_000).unwrap());
    pen(&mut e, "up", 10.0, 10.0);
    assert_eq!((strokes(&e, n), shapes(&e, n).len()), (1, 0));
    // A rectangle lifted without holding stays ink.
    let t = rough_rectangle(&mut e, 20_000_000);
    assert!(!e.tick_pen(t + 100_000).unwrap());
    pen(&mut e, "up", 100.0, 110.0);
    assert_eq!((strokes(&e, n), shapes(&e, n).len()), (2, 0));
}

fn text_of(e: &mut Engine, id: u64) -> String {
    e.execute("text.get", json!({"id": id})).unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string()
}

fn edit_id(e: &Engine) -> u64 {
    e.session.text_edit.as_ref().unwrap().id.unwrap()
}

#[test]
fn the_text_tool_types_into_a_new_box_as_one_undo_step() {
    let (mut e, n) = setup();
    e.execute("tool.select.text", json!({})).unwrap();
    pen(&mut e, "down", 100.0, 100.0);
    pen(&mut e, "up", 100.0, 100.0);
    assert!(e.session.text_edit.is_some());
    assert_eq!(
        e.session.ws.canvas(NoteId(n)).unwrap().objects.len(),
        0,
        "nothing stored before typing"
    );
    let steps = undo_len(&e);
    for t in ["H", "ä", "llo", " Wörld"] {
        e.execute("text.input", json!({"text": t})).unwrap();
    }
    let id = edit_id(&e);
    assert_eq!(text_of(&mut e, id), "Hällo Wörld");
    assert_eq!(undo_len(&e), steps + 1, "typing is one step");
    e.execute("text.input", json!({"key": "backspace"})).unwrap();
    e.execute("text.input", json!({"key": "left"})).unwrap();
    e.execute("text.input", json!({"key": "enter"})).unwrap();
    assert_eq!(text_of(&mut e, id), "Hällo Wör\nl");
    // Moving the caret starts a new step.
    assert_eq!(undo_len(&e), steps + 2);
    e.execute("text.input", json!({"key": "home", "shift": true})).unwrap();
    e.execute("text.input", json!({"text": "X"})).unwrap();
    assert_eq!(text_of(&mut e, id), "Hällo Wör\nXl");
    e.execute("text.input", json!({"key": "escape"})).unwrap();
    assert!(e.session.text_edit.is_none());
    e.execute("edit.undo", json!({})).unwrap();
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(text_of(&mut e, id), "Hällo Wörl", "backspace belonged to the typing");
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(e.session.ws.canvas(NoteId(n)).unwrap().objects.len(), 0);
}

#[test]
fn tapping_a_box_edits_it_and_styles_toggle_on_the_selection() {
    let (mut e, _) = setup();
    let id = e
        .execute(
            "text.create",
            json!({"at": [50.0, 50.0], "width": 300.0, "content": "one two"}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("tool.select.text", json!({})).unwrap();
    // The first tap selects the box (drag it, resize it), the second types into it.
    pen(&mut e, "down", 60.0, 60.0);
    pen(&mut e, "up", 60.0, 60.0);
    assert!(e.session.text_edit.is_none());
    assert_eq!(e.session.selection.ids, vec![id]);
    pen(&mut e, "down", 60.0, 60.0);
    pen(&mut e, "up", 60.0, 60.0);
    assert!(e.session.selection.ids.is_empty());
    let ed = e.session.text_edit.clone().unwrap();
    assert_eq!(ed.id, Some(id));
    assert_eq!(ed.place, Some((60.0, 60.0)), "the UI places the caret");
    e.execute("text.caret", json!({"offset": 4})).unwrap();
    e.execute("text.caret", json!({"offset": 7, "extend": true})).unwrap();
    e.execute("text.style", json!({"format": "bold"})).unwrap();
    let marks = e.execute("text.get", json!({"id": id})).unwrap()["marks"].clone();
    assert_eq!(marks, json!([{"start": 4, "end": 7, "format": "bold"}]), "{marks}");
    e.execute("text.style", json!({"format": "bold"})).unwrap();
    assert_eq!(e.execute("text.get", json!({"id": id})).unwrap()["marks"], json!([]));
    // Typing after bold text stays bold.
    e.execute("text.style", json!({"format": "bold"})).unwrap();
    e.execute("text.caret", json!({"offset": 7})).unwrap();
    e.execute("text.input", json!({"text": "s"})).unwrap();
    assert_eq!(text_of(&mut e, id), "one twos");
    let m = e.execute("text.get", json!({"id": id})).unwrap()["marks"].clone();
    assert_eq!(m[0]["end"], json!(8), "{m}");
}

#[test]
fn an_emptied_box_is_removed_when_editing_ends() {
    let (mut e, n) = setup();
    e.execute("tool.select.text", json!({})).unwrap();
    pen(&mut e, "down", 100.0, 100.0);
    pen(&mut e, "up", 100.0, 100.0);
    e.execute("text.input", json!({"text": "a"})).unwrap();
    e.execute("text.input", json!({"key": "backspace"})).unwrap();
    // Switching tools ends editing.
    e.execute("tool.select.pen", json!({})).unwrap();
    assert!(e.session.text_edit.is_none());
    assert_eq!(e.session.ws.canvas(NoteId(n)).unwrap().objects.len(), 0);
    assert!(
        e.execute("text.input", json!({"text": "x"})).is_err(),
        "nothing to type into"
    );
}

fn object(e: &Engine, n: u64, id: u64) -> vibenote_model::CanvasObject {
    e.session.ws.canvas(NoteId(n)).unwrap().objects[&ObjectId(id)].clone()
}

#[test]
fn a_lasso_tap_selects_the_object_under_it_and_text_boxes_move_and_widen() {
    let (mut e, n) = setup();
    let id = e
        .execute(
            "text.create",
            json!({"at": [100.0, 100.0], "width": 200.0, "content": "hello there"}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("tool.select.lasso", json!({})).unwrap();
    pen(&mut e, "down", 110.0, 108.0);
    pen(&mut e, "up", 111.0, 108.0);
    assert_eq!(e.session.selection.ids, vec![id], "a tap on the text selects it");
    // Drag it by its middle.
    let before = undo_len(&e);
    pen(&mut e, "down", 150.0, 108.0);
    pen(&mut e, "motion", 250.0, 158.0);
    pen(&mut e, "up", 250.0, 158.0);
    let ObjectKind::Text { origin, width, .. } = object(&e, n, id).kind else {
        panic!()
    };
    assert_eq!((origin.x, origin.y, width), (200.0, 150.0, 200.0));
    assert_eq!(undo_len(&e), before + 1);
    // Its width handle: the middle of the box's right edge.
    let r = e.session.selection_rect().unwrap();
    let handle = (r.max.x + 6.0, f64::midpoint(r.min.y, r.max.y));
    e.session.live.hover = Some(handle);
    assert_eq!(e.session.cursor_hint(), CursorHint::ResizeWidth);
    pen(&mut e, "down", handle.0, handle.1);
    pen(&mut e, "motion", handle.0 + 80.0, handle.1 + 30.0);
    pen(&mut e, "up", handle.0 + 80.0, handle.1 + 30.0);
    let ObjectKind::Text { origin, width, .. } = object(&e, n, id).kind else {
        panic!()
    };
    assert_eq!(
        (origin.x, origin.y, width),
        (200.0, 150.0, 280.0),
        "only the width changes"
    );
    e.execute("edit.undo", json!({})).unwrap();
    let ObjectKind::Text { width, .. } = object(&e, n, id).kind else {
        panic!()
    };
    assert_eq!(width, 200.0);
    // A tap on empty paper selects nothing.
    pen(&mut e, "down", 900.0, 900.0);
    pen(&mut e, "up", 900.0, 900.0);
    assert!(e.session.selection.ids.is_empty());
}

#[test]
fn images_are_selected_by_a_tap_and_dragged_with_a_preview() {
    let (mut e, n) = setup();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 30, 30, 255]))
        .save(&path)
        .unwrap();
    let id = e
        .execute("image.import", json!({"path": path, "at": [100.0, 100.0]}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("tool.select.lasso", json!({})).unwrap();
    mouse(&mut e, "down", 120.0, 110.0);
    mouse(&mut e, "up", 120.0, 110.0);
    assert_eq!(e.session.selection.ids, vec![id]);
    let shown = |e: &Engine| {
        e.session
            .images_in_view((0.0, 0.0), 1.0, (1000.0, 1000.0), true)
            .into_iter()
            .map(|p| (p.rect.min.x, p.rect.min.y))
            .collect::<Vec<_>>()
    };
    assert_eq!(shown(&e), vec![(100.0, 100.0)]);
    mouse(&mut e, "down", 120.0, 110.0);
    mouse(&mut e, "motion", 320.0, 160.0);
    assert_eq!(shown(&e), vec![(300.0, 150.0)], "the image follows while dragged");
    mouse(&mut e, "up", 320.0, 160.0);
    let ObjectKind::Image { origin, .. } = object(&e, n, id).kind else {
        panic!()
    };
    assert_eq!((origin.x, origin.y), (300.0, 150.0));
}

#[test]
fn tidy_prettifies_the_selected_handwriting_and_reverts_it() {
    let (mut e, n) = setup();
    // Six short "letters" on a line tilted by 20°.
    let tilt = 20f64.to_radians();
    let mut ids = Vec::new();
    for k in 0..6 {
        let samples: Vec<Value> = (0..12)
            .map(|i| {
                let (x, y) = (f64::from(k) * 60.0 + f64::from(i % 3) * 4.0, f64::from(i) * 2.0);
                json!({"x": 500.0 + x * tilt.cos() - y * tilt.sin(), "y": 500.0 + x * tilt.sin() + y * tilt.cos(), "p": 0.5})
            })
            .collect();
        ids.push(
            e.execute("ink.stroke.add", json!({ "samples": samples })).unwrap()["id"]
                .as_u64()
                .unwrap(),
        );
    }
    e.execute("selection.set", json!({ "ids": ids })).unwrap();
    assert_eq!(e.session.selection_ink(), (true, false));
    let ws = e.session.ws.canvas(NoteId(n)).unwrap().clone();
    let steps = undo_len(&e);
    e.execute("selection.tidy", json!({})).unwrap();
    assert_eq!(e.session.selection_ink(), (true, true));
    assert_eq!(e.session.display_transforms(NoteId(n)).len(), 6);
    assert_eq!(
        e.session.ws.canvas(NoteId(n)).unwrap(),
        &ws,
        "the ink itself never changes"
    );
    assert_eq!(undo_len(&e), steps + 1);
    // Tidy again (e.g. with more strokes): still one block.
    e.execute("selection.tidy", json!({})).unwrap();
    assert_eq!(e.session.ws.prettified[&NoteId(n)].len(), 1);
    e.execute("selection.tidy", json!({"revert": true})).unwrap();
    assert!(e.session.display_transforms(NoteId(n)).is_empty());
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(e.session.display_transforms(NoteId(n)).len(), 6);
    // Nothing but text selected: an error, nothing changes.
    let t = e
        .execute("text.create", json!({"at": [0.0, 0.0], "width": 100.0, "content": "x"}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("selection.set", json!({ "ids": [t] })).unwrap();
    assert!(e.execute("selection.tidy", json!({})).is_err());
}

#[test]
fn holding_the_mouse_still_snaps_a_shape_too() {
    let (mut e, n) = setup();
    let mut events = Vec::new();
    let mut t = 1_000_000u64;
    let mut push = |kind: &str, x: f64, y: f64, t: u64| {
        events.push(json!({"t_us": t, "kind": kind, "x": x, "y": y, "button": 272}))
    };
    push("down", 100.0, 100.0, t);
    for (a, b) in [
        ((100.0, 100.0), (300.0, 100.0)),
        ((300.0, 100.0), (300.0, 200.0)),
        ((300.0, 200.0), (100.0, 200.0)),
        ((100.0, 200.0), (100.0, 102.0)),
    ] {
        for i in 1..=20 {
            t += 10_000;
            let f = f64::from(i) / 20.0;
            push("motion", a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, t);
        }
    }
    e.execute("input.pointer", json!({ "events": events })).unwrap();
    assert!(e.tick_pen(t + 600_000).unwrap());
    assert_eq!(shapes(&e, n).len(), 1);
    mouse(&mut e, "up", 100.0, 102.0);
    assert_eq!(strokes(&e, n), 0);
}
