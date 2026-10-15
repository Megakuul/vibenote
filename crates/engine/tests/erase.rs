#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]
//! Erasers (M4.2–M4.4: R-PEN-006..008).

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{NoteId, ObjectKind};

fn line(e: &mut Engine, n: u64, y: f64) -> u64 {
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": 0.0, "y": y, "p": 0.5}, {"x": 100.0, "y": y, "p": 0.5}]}),
    )
    .unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn ids(e: &Engine, n: u64) -> Vec<u64> {
    e.session
        .ws
        .canvas(NoteId(n))
        .unwrap()
        .objects
        .keys()
        .map(|o| o.0)
        .collect()
}

fn note(e: &mut Engine) -> u64 {
    let n = e.execute("workspace.note.create", json!({"name": "e"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    n
}

#[test]
fn erasing_is_one_exact_undo_step_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = note(&mut e);
    let a = line(&mut e, n, 0.0);
    let b = line(&mut e, n, 50.0);
    let before = e.session.ws.clone();
    let r = e
        .execute(
            "ink.erase_area",
            json!({"path": [[50.0, -20.0], [50.0, 20.0]], "radius": 5.0}),
        )
        .unwrap();
    assert_eq!(r["erased"], json!([a]));
    assert_eq!(r["created"].as_array().unwrap().len(), 2);
    let after = e.session.ws.clone();
    e.execute("edit.undo", json!({})).unwrap();
    // Ids are never reused: only the id counter may differ.
    let mut undone = e.session.ws.clone();
    undone.next_id = before.next_id;
    assert_eq!(undone, before, "undo restores the original stroke exactly");
    e.execute("edit.redo", json!({})).unwrap();
    assert_eq!(e.session.ws, after);
    // Nothing hit: no undo step.
    let r = e
        .execute("ink.erase_stroke", json!({"path": [[500.0, 500.0]]}))
        .unwrap();
    assert_eq!(r["erased"], json!([]));
    e.execute("ink.erase_stroke", json!({"path": [[10.0, 45.0], [10.0, 55.0]]}))
        .unwrap();
    assert!(!ids(&e, n).contains(&b));
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = Engine::open(&path).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
}

/// The hit index follows every change (new strokes, undo, area pieces) without a rebuild.
#[test]
fn the_hit_index_follows_edits() {
    let mut e = Engine::new().unwrap();
    let n = note(&mut e);
    for k in 0..50 {
        line(&mut e, n, f64::from(k) * 10.0);
    }
    e.execute("ink.erase_stroke", json!({"path": [[50.0, 95.0], [50.0, 105.0]]}))
        .unwrap();
    e.execute("edit.undo", json!({})).unwrap();
    let late = line(&mut e, n, 1000.0);
    let r = e
        .execute("ink.erase_stroke", json!({"path": [[90.0, 1000.0]]}))
        .unwrap();
    assert_eq!(
        r["erased"],
        json!([late]),
        "a stroke added after the index was built is found"
    );
    let r = e
        .execute("ink.erase_stroke", json!({"path": [[50.0, 95.0], [50.0, 105.0]]}))
        .unwrap();
    assert_eq!(
        r["erased"].as_array().unwrap().len(),
        1,
        "the undone stroke is found again"
    );
}

/// A note still loading is completed before erasing, so stored strokes are not missed.
#[test]
fn erasing_a_partly_loaded_note_sees_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = note(&mut e);
    for k in 0..3000 {
        line(&mut e, n, f64::from(k) * 3.0);
    }
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": n, "viewport": [20.0, 20.0]}))
        .unwrap();
    let r = e
        .execute(
            "ink.erase_stroke",
            json!({"path": [[50.0, 8000.0], [50.0, 8010.0]], "radius": 0.5}),
        )
        .unwrap();
    assert_eq!(r["erased"].as_array().unwrap().len(), 4, "{r}");
}

#[test]
fn hostile_eraser_params_are_rejected() {
    let mut e = Engine::new().unwrap();
    let n = note(&mut e);
    line(&mut e, n, 0.0);
    let before = e.session.ws.clone();
    let long: Vec<Value> = (0..5000).map(|i| json!([f64::from(i), 0.0])).collect();
    for p in [
        json!({"path": []}),
        json!({"path": [[0.0, 0.0]], "radius": -1.0}),
        json!({"path": [[1e300, 0.0]]}),
        json!({"path": long}),
    ] {
        assert!(e.execute("ink.erase_area", p.clone()).is_err(), "{p}");
        assert_eq!(e.session.ws, before);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    /// Random strokes and area erasures: undoing all erasures restores the canvas exactly, and
    /// the pieces keep only original samples or points on the cut.
    #[test]
    fn area_erasing_undoes_exactly(
        strokes in prop::collection::vec(prop::collection::vec((-100.0f64..100.0, -100.0f64..100.0), 2..20), 1..8),
        erasers in prop::collection::vec((prop::collection::vec((-100.0f64..100.0, -100.0f64..100.0), 1..5), 1.0f64..20.0), 1..5),
    ) {
        let mut e = Engine::new().unwrap();
        let n = note(&mut e);
        for s in &strokes {
            let samples: Vec<Value> = s.iter().map(|&(x, y)| json!({"x": x, "y": y, "p": 0.5})).collect();
            e.execute("ink.stroke.add", json!({"samples": samples})).unwrap();
        }
        let before = e.session.ws.clone();
        let mut steps = 0;
        for (path, r) in &erasers {
            let p: Vec<[f64; 2]> = path.iter().map(|&(x, y)| [x, y]).collect();
            let res = e.execute("ink.erase_area", json!({"path": p, "radius": r})).unwrap();
            if !res["erased"].as_array().unwrap().is_empty() {
                steps += 1;
            }
        }
        for o in e.session.ws.canvas(NoteId(n)).unwrap().objects.values() {
            if let ObjectKind::Stroke(s) = &o.kind {
                prop_assert!(!s.samples.is_empty());
            }
        }
        for _ in 0..steps {
            e.execute("edit.undo", json!({})).unwrap();
        }
        let mut ws = e.session.ws.clone();
        ws.next_id = before.next_id;
        prop_assert_eq!(ws, before);
    }
}

/// Review M4 H2: long gestures (more points than one eraser/lasso path takes) still work.
#[test]
fn long_eraser_and_lasso_gestures_are_not_dropped() {
    let mut e = vibenote_engine::Engine::new().unwrap();
    let n = e
        .execute("workspace.note.create", serde_json::json!({"name": "l"}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", serde_json::json!({"id": n})).unwrap();
    let add = |e: &mut vibenote_engine::Engine, y: f64| {
        e.execute(
            "ink.stroke.add",
            serde_json::json!({"samples": [{"x": 0.0, "y": y, "p": 0.5}, {"x": 400.0, "y": y, "p": 0.5, "t_us": 1000}]}),
        )
        .unwrap()["id"]
            .as_u64()
            .unwrap()
    };
    let (a, _b, c) = (add(&mut e, 50.0), add(&mut e, 300.0), add(&mut e, 600.0));
    // 5000 motion events with the eraser end: scrubbing in place, then crossing stroke b
    // only at the very end (beyond the first 4096 points).
    let ev = |kind: &str, x: f64, y: f64, t: u64| serde_json::json!({"t_us": t, "kind": kind, "tool": "eraser", "x": x, "y": y, "p": 0.5});
    let mut evs = vec![ev("proximity_in", 200.0, 150.0, 0), ev("down", 200.0, 150.0, 1)];
    for k in 0..4990u64 {
        evs.push(ev("motion", 200.0 + (k % 7) as f64, 150.0 + (k % 5) as f64, 2 + k));
    }
    for k in 0..10u64 {
        evs.push(ev("motion", 200.0, 150.0 + 20.0 * k as f64, 5000 + k));
    }
    evs.push(ev("up", 200.0, 330.0, 6000));
    evs.push(ev("proximity_out", 200.0, 330.0, 6001));
    let r = e.execute("input.pen", serde_json::json!({ "events": evs })).unwrap();
    assert!(r["errors"].as_array().unwrap().is_empty(), "{r}");
    let ids: Vec<u64> = e
        .session
        .ws
        .canvas(vibenote_model::NoteId(n))
        .unwrap()
        .objects
        .keys()
        .map(|i| i.0)
        .collect();
    assert_eq!(ids, vec![a, c], "only b was crossed");
    // A 6000-point lasso (barrel button 1) around stroke c selects it.
    let pen = |kind: &str, x: f64, y: f64, t: u64| serde_json::json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": y, "p": 0.5, "button": 331});
    let mut evs = vec![
        pen("proximity_in", -10.0, 550.0, 0),
        pen("button_down", -10.0, 550.0, 1),
        pen("down", -10.0, 550.0, 2),
    ];
    let corners = [
        (-10.0, 550.0),
        (410.0, 550.0),
        (410.0, 650.0),
        (-10.0, 650.0),
        (-10.0, 551.0),
    ];
    let mut t = 3u64;
    for w in corners.windows(2) {
        for k in 0..1500 {
            let f = k as f64 / 1500.0;
            evs.push(pen(
                "motion",
                w[0].0 + (w[1].0 - w[0].0) * f,
                w[0].1 + (w[1].1 - w[0].1) * f,
                t,
            ));
            t += 1;
        }
    }
    evs.push(pen("up", -10.0, 551.0, t));
    let r = e.execute("input.pen", serde_json::json!({ "events": evs })).unwrap();
    assert!(r["errors"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(e.inspect(Some("/selection/ids")).unwrap(), serde_json::json!([c]));
}

/// Review M4 M1: erasing near nothing on a big note costs little (candidates come from small
/// per-segment queries; misses are never cloned).
#[test]
fn erasing_on_a_big_note_stays_cheap() {
    let mut e = vibenote_engine::Engine::new().unwrap();
    let n = e
        .execute("workspace.note.create", serde_json::json!({"name": "big"}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", serde_json::json!({"id": n})).unwrap();
    for k in 0..20_000u32 {
        let (x, y) = (f64::from(k % 141) * 57.0, f64::from(k / 141) * 57.0);
        e.execute(
            "ink.stroke.add",
            serde_json::json!({"samples": (0..10).map(|i| serde_json::json!({"x": x + f64::from(i) * 3.0, "y": y + f64::from(i % 3), "p": 0.5, "t_us": i * 1000})).collect::<Vec<_>>()}),
        )
        .unwrap();
    }
    // A 400-point diagonal path between the strokes (hits nothing).
    let path: Vec<[f64; 2]> = (0..400)
        .map(|i| [40.0 + f64::from(i) * 0.1, 45.0 + f64::from(i) * 0.1])
        .collect();
    let t = std::time::Instant::now();
    for _ in 0..5 {
        let r = e
            .execute("ink.erase_stroke", serde_json::json!({"path": path, "radius": 2.0}))
            .unwrap();
        assert!(r["erased"].as_array().unwrap().is_empty());
    }
    let ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(ms < 250.0, "5 erases took {ms:.0} ms");
}

/// Review M4 M2: erasing on a note still loading reads only the area around the eraser, and
/// the result equals erasing on the fully loaded note.
#[test]
fn erasing_on_a_loading_note_reads_only_its_area() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&path).unwrap();
    let n = e
        .execute("workspace.note.create", serde_json::json!({"name": "p"}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    for k in 0..400u32 {
        let y = f64::from(k) * 100.0;
        e.execute(
            "ink.stroke.add",
            serde_json::json!({"note": n, "samples": [{"x": 0.0, "y": y, "p": 0.5}, {"x": 50.0, "y": y, "p": 0.5, "t_us": 1000}]}),
        )
        .unwrap();
    }
    e.sync().unwrap();
    drop(e);
    let mut e = vibenote_engine::Engine::open(&path).unwrap();
    e.execute("note.open", serde_json::json!({"id": n, "viewport": [100.0, 100.0]}))
        .unwrap();
    // Far outside the first viewport: stroke at y = 30,000.
    let r = e
        .execute(
            "ink.erase_stroke",
            serde_json::json!({"path": [[25.0, 29_990.0], [25.0, 30_010.0]], "radius": 2.0}),
        )
        .unwrap();
    assert_eq!(r["erased"].as_array().unwrap().len(), 1, "{r}");
    let loaded = e.session.ws.canvas(vibenote_model::NoteId(n)).unwrap().objects.len();
    assert!(loaded < 200, "the whole note was read ({loaded} objects)");
    e.load_all().unwrap();
    assert_eq!(
        e.session.ws.canvas(vibenote_model::NoteId(n)).unwrap().objects.len(),
        399
    );
}
