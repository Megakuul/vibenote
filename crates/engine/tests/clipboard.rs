#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! In-app clipboard (M4.9, R-UNDO-002): copy/cut/paste within and between notes.

use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{CanvasObject, NoteId, ObjectKind};

fn note(e: &mut Engine, name: &str) -> u64 {
    e.execute("workspace.note.create", json!({ "name": name })).unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn stroke(e: &mut Engine, n: u64, x: f64, y: f64, color: u32) -> u64 {
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "color": color, "samples": [{"x": x, "y": y, "p": 0.5}, {"x": x + 10.0, "y": y + 5.0, "p": 0.7, "t_us": 1000}]}),
    )
    .unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn objs(e: &Engine, n: u64) -> Vec<CanvasObject> {
    let mut v: Vec<_> = e
        .session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .values()
        .cloned()
        .collect();
    v.sort_by_key(|o| (o.z, o.id));
    v
}

fn first_point(o: &CanvasObject) -> (f64, f64) {
    match &o.kind {
        ObjectKind::Stroke(s) => (s.samples[0].x, s.samples[0].y),
        _ => panic!("not a stroke"),
    }
}

fn ids(v: &Value) -> Vec<u64> {
    v["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect()
}

#[test]
fn copy_and_paste_between_notes_keeps_everything_but_ids_and_position() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    let (a, b) = (note(&mut e, "a"), note(&mut e, "b"));
    let s1 = stroke(&mut e, a, 10.0, 20.0, 0xff0000ff);
    let s2 = stroke(&mut e, a, 50.0, 40.0, 0x0000ffff);
    e.execute("note.open", json!({"id": a})).unwrap();
    e.execute("selection.set", json!({"ids": [s2, s1]})).unwrap();
    assert_eq!(e.execute("clipboard.copy", json!({})).unwrap()["count"], json!(2));
    assert_eq!(e.inspect(Some("/clipboard/count")).unwrap(), json!(2));
    // Later edits of the originals do not change the clipboard.
    e.execute("selection.recolor", json!({"color": 0x00ff00ff})).unwrap();
    e.execute("note.open", json!({"id": b})).unwrap();
    let stray = stroke(&mut e, b, 0.0, 0.0, 0x000000ff);
    let r = e.execute("clipboard.paste", json!({"at": [100.0, 200.0]})).unwrap();
    let pasted = ids(&r);
    assert_eq!(pasted.len(), 2);
    assert!(pasted.iter().all(|id| *id != s1 && *id != s2));
    let got = objs(&e, b);
    assert_eq!(got.len(), 3);
    // On top, in the original order; the bounds' top-left lands at `at`.
    assert_eq!(got[0].id.0, stray);
    assert_eq!(got[1..].iter().map(|o| o.id.0).collect::<Vec<_>>(), pasted);
    assert_eq!(first_point(&got[1]), (100.0, 200.0));
    assert_eq!(first_point(&got[2]), (140.0, 220.0));
    let colors: Vec<u32> = got[1..]
        .iter()
        .map(|o| match &o.kind {
            ObjectKind::Stroke(s) => s.color.0,
            _ => 0,
        })
        .collect();
    assert_eq!(colors, vec![0xff0000ff, 0x0000ffff]);
    // The pasted objects are the selection; one undo removes them all.
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), json!(pasted));
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(objs(&e, b).len(), 1);
    // Stored like any other ink.
    e.execute("edit.redo", json!({})).unwrap();
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    e.load_all().unwrap();
    assert_eq!(objs(&e, b).len(), 3);
}

#[test]
fn pasting_back_into_the_source_note_cascades_and_elsewhere_keeps_the_position() {
    let mut e = Engine::new().unwrap();
    let (a, b) = (note(&mut e, "a"), note(&mut e, "b"));
    e.execute("note.open", json!({"id": a})).unwrap();
    let s = stroke(&mut e, a, 10.0, 20.0, 0x000000ff);
    e.execute("selection.set", json!({"ids": [s]})).unwrap();
    e.execute("clipboard.copy", json!({})).unwrap();
    for k in 1..=2 {
        let r = e.execute("clipboard.paste", json!({})).unwrap();
        let o = objs(&e, a).into_iter().find(|o| o.id.0 == ids(&r)[0]).unwrap();
        assert_eq!(
            first_point(&o),
            (10.0 + 24.0 * f64::from(k), 20.0 + 24.0 * f64::from(k))
        );
    }
    let r = e.execute("clipboard.paste", json!({"note": b})).unwrap();
    let o = objs(&e, b).into_iter().find(|o| o.id.0 == ids(&r)[0]).unwrap();
    assert_eq!(first_point(&o), (10.0, 20.0));
}

#[test]
fn cut_removes_in_one_step_and_the_clipboard_survives_undo() {
    let mut e = Engine::new().unwrap();
    let a = note(&mut e, "a");
    e.execute("note.open", json!({"id": a})).unwrap();
    let s1 = stroke(&mut e, a, 0.0, 0.0, 0x000000ff);
    let s2 = stroke(&mut e, a, 30.0, 0.0, 0x000000ff);
    let keep = stroke(&mut e, a, 60.0, 0.0, 0x000000ff);
    e.execute("selection.set", json!({"ids": [s1, s2]})).unwrap();
    assert_eq!(e.execute("clipboard.cut", json!({})).unwrap()["count"], json!(2));
    assert_eq!(objs(&e, a).iter().map(|o| o.id.0).collect::<Vec<_>>(), vec![keep]);
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), json!([]));
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(objs(&e, a).len(), 3);
    e.execute("clipboard.paste", json!({"at": [0.0, 100.0]})).unwrap();
    assert_eq!(objs(&e, a).len(), 5);
}

#[test]
fn errors_change_nothing() {
    let mut e = Engine::new().unwrap();
    let a = note(&mut e, "a");
    e.execute("note.open", json!({"id": a})).unwrap();
    assert!(e.execute("clipboard.paste", json!({})).is_err(), "empty clipboard");
    assert!(e.execute("clipboard.copy", json!({})).is_err(), "nothing selected");
    assert!(e.execute("clipboard.cut", json!({})).is_err(), "nothing selected");
    let s = stroke(&mut e, a, 0.0, 0.0, 0x000000ff);
    e.execute("selection.set", json!({"ids": [s]})).unwrap();
    e.execute("clipboard.copy", json!({})).unwrap();
    let before = e.inspect(None).unwrap();
    for at in [
        json!([f64::MAX, 0.0]),
        json!([1.0e12, 0.0]),
        json!(["x", 0.0]),
        json!([1.0]),
    ] {
        assert!(e.execute("clipboard.paste", json!({ "at": at })).is_err(), "{at}");
    }
    assert!(e.execute("clipboard.paste", json!({"note": 999_999})).is_err());
    assert_eq!(e.inspect(None).unwrap(), before);
}
