#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Integration tests may unwrap/index/panic (AGENTS.md §3 applies to non-test code).
//! Engine harness tests: command flow, undo identity, hostile params.

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::{Engine, EngineError};

fn id_of(v: &Value) -> u64 {
    v["id"].as_u64().expect("id")
}

#[test]
fn folder_note_stroke_undo_redo_flow() {
    let mut e = Engine::new().unwrap();
    let f = id_of(
        &e.execute(
            "workspace.folder.create",
            json!({"name": "Analysis", "color": 0xd6604dff_u32}),
        )
        .unwrap(),
    );
    let n = id_of(
        &e.execute("workspace.note.create", json!({"folder": f, "name": "Vorlesung 1"}))
            .unwrap(),
    );
    e.execute("note.open", json!({"id": n})).unwrap();
    let s = id_of(
        &e.execute(
            "ink.stroke.add",
            json!({"samples": [{"x": 0.0, "y": 0.0, "p": 0.2, "tx": 15.0, "ty": -10.0, "t_us": 0},
                               {"x": 10.0, "y": 5.0, "p": 0.8, "tx": 15.0, "ty": -10.0, "t_us": 5000}]}),
        )
        .unwrap(),
    );
    let st = e.inspect(Some(&format!("/ws/canvases/{n}/objects/{s}"))).unwrap();
    assert_eq!(st["kind"], "stroke");
    assert_eq!(st["samples"][1]["pressure"].as_f64().unwrap() as f32, 0.8);
    assert_eq!(st["samples"][1]["t_us"], 5000);
    // undo the stroke (note scope), then redo
    assert_eq!(e.execute("edit.undo", Value::Null).unwrap()["label"], "stroke");
    assert!(e.inspect(Some(&format!("/ws/canvases/{n}/objects/{s}"))).is_err());
    e.execute("edit.redo", Value::Null).unwrap();
    assert!(e.inspect(Some(&format!("/ws/canvases/{n}/objects/{s}"))).is_ok());
    // nothing more to redo
    assert_eq!(e.execute("edit.redo", Value::Null), Err(EngineError::NothingTo("redo")));
    // workspace-scope undo removes the note again
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert!(e.inspect(Some(&format!("/ws/notes/{n}"))).is_err());
}

#[test]
fn folder_cycle_and_non_empty_delete_are_errors_not_panics() {
    let mut e = Engine::new().unwrap();
    let a = id_of(&e.execute("workspace.folder.create", json!({"name": "a"})).unwrap());
    let b = id_of(
        &e.execute("workspace.folder.create", json!({"parent": a, "name": "b"}))
            .unwrap(),
    );
    assert!(matches!(
        e.execute("workspace.folder.move", json!({"id": a, "parent": b})),
        Err(EngineError::Model(_))
    ));
    assert!(matches!(
        e.execute("workspace.folder.delete", json!({"id": a})),
        Err(EngineError::Model(_))
    ));
}

#[test]
fn unknown_and_invalid_are_reported() {
    let mut e = Engine::new().unwrap();
    assert_eq!(
        e.execute("nope", Value::Null),
        Err(EngineError::UnknownCommand("nope".into()))
    );
    assert!(matches!(
        e.execute("workspace.folder.create", json!({"nam": "x"})),
        Err(EngineError::InvalidParams { .. })
    ));
    assert!(matches!(
        e.execute("ink.stroke.add", json!({"samples": []})),
        Err(EngineError::Model(_))
    ));
}

#[test]
fn stubs_are_registered_and_return_not_implemented() {
    let mut e = Engine::new().unwrap();
    // (Updated 2026-10-13 with the owner's approval: the count of stubs shrinks as features
    // land; the registry as a whole must not lose commands.)
    assert!(
        e.registry.specs().count() >= 69,
        "commands disappeared from the registry"
    );
    let stubs: Vec<&str> = e.registry.specs().filter(|s| !s.implemented).map(|s| s.id).collect();
    for id in stubs {
        assert_eq!(e.execute(id, Value::Null), Err(EngineError::NotImplemented(id.into())));
    }
}

#[test]
fn every_command_declares_requirements() {
    let e = Engine::new().unwrap();
    for s in e.registry.specs() {
        assert!(!s.reqs.is_empty(), "{} has no requirement ids", s.id);
        assert!(s.reqs.iter().all(|r| r.starts_with("R-")), "{}: {:?}", s.id, s.reqs);
    }
}

// ---------- property: random command sequences, then full undo restores the initial state ----------

#[derive(Debug, Clone)]
enum Act {
    Folder(Option<usize>, String),
    Note(Option<usize>, String),
    Rename(usize, String),
    MoveFolder(usize, Option<usize>),
    DelNote(usize),
    DelFolder(usize),
    Stroke(usize, Vec<(f64, f64, f32)>),
    DelObj(usize, usize),
    Undo(Option<usize>),
    Redo(Option<usize>),
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        (proptest::option::of(0usize..6), "[a-z]{0,6}").prop_map(|(p, n)| Act::Folder(p, n)),
        (proptest::option::of(0usize..6), "[a-z]{0,6}").prop_map(|(p, n)| Act::Note(p, n)),
        (0usize..6, ".{0,8}").prop_map(|(i, n)| Act::Rename(i, n)),
        (0usize..6, proptest::option::of(0usize..6)).prop_map(|(a, b)| Act::MoveFolder(a, b)),
        (0usize..6).prop_map(Act::DelNote),
        (0usize..6).prop_map(Act::DelFolder),
        (
            0usize..6,
            prop::collection::vec((-1e6f64..1e6, -1e6f64..1e6, 0f32..=1.0), 1..8)
        )
            .prop_map(|(n, p)| Act::Stroke(n, p)),
        (0usize..6, 0usize..6).prop_map(|(n, o)| Act::DelObj(n, o)),
        proptest::option::of(0usize..6).prop_map(Act::Undo),
        proptest::option::of(0usize..6).prop_map(Act::Redo),
    ]
}

fn ids(e: &Engine, ptr: &str) -> Vec<u64> {
    e.inspect(Some(ptr))
        .ok()
        .and_then(|v| v.as_object().map(|m| m.keys().filter_map(|k| k.parse().ok()).collect()))
        .unwrap_or_default()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn random_commands_then_full_undo_restore_initial_state(acts in prop::collection::vec(act(), 1..50)) {
        let mut e = Engine::new().unwrap();
        let initial = e.session.ws.clone();
        for a in acts {
            let folders = ids(&e, "/ws/folders");
            let notes = ids(&e, "/ws/notes");
            let pick = |v: &Vec<u64>, i: usize| v.get(i % v.len().max(1)).copied();
            let (cmd, params) = match a {
                Act::Folder(p, n) => ("workspace.folder.create", json!({"parent": p.and_then(|i| pick(&folders, i)), "name": n})),
                Act::Note(p, n) => ("workspace.note.create", json!({"folder": p.and_then(|i| pick(&folders, i)), "name": n})),
                Act::Rename(i, n) => ("workspace.note.rename", json!({"id": pick(&notes, i).unwrap_or(999_999), "name": n})),
                Act::MoveFolder(a, b) => ("workspace.folder.move", json!({"id": pick(&folders, a).unwrap_or(999_999), "parent": b.and_then(|i| pick(&folders, i))})),
                Act::DelNote(i) => ("workspace.note.delete", json!({"id": pick(&notes, i).unwrap_or(999_999)})),
                Act::DelFolder(i) => ("workspace.folder.delete", json!({"id": pick(&folders, i).unwrap_or(999_999)})),
                Act::Stroke(n, pts) => ("ink.stroke.add", json!({"note": pick(&notes, n).unwrap_or(999_999),
                    "samples": pts.iter().map(|&(x, y, p)| json!({"x": x, "y": y, "p": p})).collect::<Vec<_>>()})),
                Act::DelObj(n, o) => {
                    let note = pick(&notes, n).unwrap_or(999_999);
                    let objs = ids(&e, &format!("/ws/canvases/{note}/objects"));
                    // several ids per step (multi-op undo steps)
                    let many: Vec<u64> = objs.iter().copied().skip(o % objs.len().max(1)).take(1 + o % 3).collect();
                    let ids = if many.is_empty() { vec![999_999] } else { many };
                    ("object.delete", json!({"note": note, "ids": ids}))
                }
                Act::Undo(n) => ("edit.undo", json!({"scope": n.and_then(|i| pick(&notes, i)).map(|x| json!(x)).unwrap_or(json!("workspace"))})),
                Act::Redo(n) => ("edit.redo", json!({"scope": n.and_then(|i| pick(&notes, i)).map(|x| json!(x)).unwrap_or(json!("workspace"))})),
            };
            let _ = e.execute(cmd, params); // errors are fine; panics are not
        }
        // Undo everything: repeatedly undo any scope that can still undo, notes first
        // (note steps only exist while their note exists), until no progress.
        loop {
            let mut progress = false;
            for n in ids(&e, "/ws/notes") {
                while e.execute("edit.undo", json!({"scope": n})).is_ok() { progress = true; }
            }
            if e.execute("edit.undo", json!({"scope": "workspace"})).is_ok() { progress = true; }
            if !progress { break; }
        }
        let mut ws = e.session.ws.clone();
        ws.next_id = initial.next_id;
        // Strokes of notes that were deleted and restored are restored with the note's canvas,
        // so after all undos the workspace must be exactly empty again.
        prop_assert_eq!(ws, initial);
    }

    #[test]
    fn hostile_params_never_panic(idx in 0usize..200, params in arb_json()) {
        let mut e = Engine::new().unwrap();
        let _ = e.execute("workspace.note.create", json!({"name": "n"}));
        let all: Vec<&'static str> = e.registry.ids().collect();
        let id = all[idx % all.len()];
        let _ = e.execute(id, params);
    }
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|i| json!(i)),
        any::<f64>().prop_map(|f| serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null)),
        ".{0,20}".prop_map(Value::String),
        Just(json!(1e308)),
        Just(json!(u64::MAX)),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("id".to_string()),
                    Just("name".to_string()),
                    Just("samples".to_string()),
                    Just("x".to_string()),
                    Just("note".to_string()),
                    Just("parent".to_string()),
                    ".{0,5}".prop_map(|s| s)
                ],
                inner,
                0..6
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Multi-op undo steps (found by harness self-validation: single-op steps hid a broken undo).
#[test]
fn multi_object_delete_undo_restores_all() {
    let mut e = Engine::new().unwrap();
    let n = id_of(&e.execute("workspace.note.create", json!({"name": "m"})).unwrap());
    e.execute("note.open", json!({"id": n})).unwrap();
    let mut ids = vec![];
    for i in 0..5 {
        let r = e
            .execute(
                "ink.stroke.add",
                json!({"samples": [{"x": f64::from(i), "y": 0.0}, {"x": f64::from(i) + 1.0, "y": 1.0}]}),
            )
            .unwrap();
        ids.push(id_of(&r));
    }
    let before = e.inspect(Some(&format!("/ws/canvases/{n}"))).unwrap();
    e.execute("object.delete", json!({"ids": ids})).unwrap();
    assert_eq!(
        e.inspect(Some(&format!("/ws/canvases/{n}/objects"))).unwrap(),
        json!({})
    );
    e.execute("edit.undo", Value::Null).unwrap();
    assert_eq!(
        e.inspect(Some(&format!("/ws/canvases/{n}"))).unwrap(),
        before,
        "undo must restore every object of the step"
    );
    e.execute("edit.redo", Value::Null).unwrap();
    assert_eq!(
        e.inspect(Some(&format!("/ws/canvases/{n}/objects"))).unwrap(),
        json!({})
    );
}

/// A batch with an invalid event is rejected as a whole: nothing in it is applied.
#[test]
fn invalid_pen_batch_changes_nothing() {
    let mut e = Engine::new().unwrap();
    let n = id_of(&e.execute("workspace.note.create", json!({"name": "p"})).unwrap());
    e.execute("note.open", json!({"id": n})).unwrap();
    let ev =
        |t: u64, kind: &str, x: f64, p: f32| json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": 5.0, "p": p});
    let before = e.inspect(None).unwrap();
    let r = e.execute(
        "input.pen",
        json!({"events": [ev(0, "down", 1.0, 0.5), ev(1000, "motion", 2.0, 7.0), ev(2000, "up", 3.0, 0.0)]}),
    );
    assert!(r.is_err());
    assert_eq!(e.inspect(None).unwrap(), before);
}
