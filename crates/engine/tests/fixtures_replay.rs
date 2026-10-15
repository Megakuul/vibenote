#![allow(
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Replays every committed ink fixture (`tests/fixtures/ink/*.jsonl`) through `input.pen`.
//! Checks: deterministic replay, one stroke per pen down (pen tool), every contact sample kept
//! exactly (R-PEN-004), eraser strokes erase the pen strokes they pass over (R-PEN-006, stroke
//! eraser by default) without errors. (Updated 2026-10-13 with the owner's approval: until M4.2
//! erasers were reported as not implemented.)

use serde_json::{Value, json};
use std::path::PathBuf;
use vibenote_engine::Engine;
use vibenote_ink::input::{Fixture, PenEventKind, PenTool};

fn fixtures() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ink");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    v.sort();
    assert!(v.len() >= 10, "expected committed fixtures in {}", dir.display());
    v
}

fn replay(f: &Fixture, batch: usize) -> (Engine, Vec<Value>) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "replay"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    let mut results = vec![];
    for chunk in f.events.chunks(batch) {
        results.push(e.execute("input.pen", json!({"events": chunk})).unwrap());
    }
    (e, results)
}

/// Expected strokes from the raw events: for each pen-tool down, the samples are the down frame
/// plus every motion frame until up / proximity-out. An eraser stroke removes the earlier pen
/// strokes whose ink it passes over (computed here independently of the engine).
type Pts = Vec<(f64, f64, f32)>;

/// Eraser disc radius at zoom 1 (the app's eraser: 8 logical px).
const ERASER_RADIUS: f64 = 8.0;

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - dx * t).hypot(p.1 - a.1 - dy * t)
}

/// Distance between segments a–b and c–d (0 when they cross).
fn segs_dist(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> f64 {
    let cross = |o: (f64, f64), p: (f64, f64), q: (f64, f64)| (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0);
    let (d1, d2, d3, d4) = (cross(c, d, a), cross(c, d, b), cross(a, b, c), cross(a, b, d));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    seg_dist(a, c, d)
        .min(seg_dist(b, c, d))
        .min(seg_dist(c, a, b))
        .min(seg_dist(d, a, b))
}

/// Does the eraser path pass over the stroke's ink?
fn erased_by(stroke: &Pts, eraser: &Pts) -> bool {
    type Seg = ((f64, f64), (f64, f64), f32);
    let pairs = |s: &Pts| -> Vec<Seg> {
        let mut v = vec![];
        for (i, &(x, y, p)) in s.iter().enumerate() {
            let (px, py, pp) = if i == 0 { (x, y, p) } else { s[i - 1] };
            v.push(((px, py), (x, y), pp.max(p)));
        }
        v
    };
    pairs(stroke).iter().any(|&(a, b, p)| {
        let r = f64::from(vibenote_ink::pressure_width(2.0, p)) * 0.5;
        pairs(eraser)
            .iter()
            .any(|&(c, d, _)| segs_dist(a, b, c, d) <= ERASER_RADIUS + r)
    })
}

fn expected(f: &Fixture) -> (Vec<Pts>, usize) {
    let mut strokes = vec![];
    let mut cur: Option<(PenTool, Pts)> = None;
    let mut erasers = 0;
    let close = |cur: &mut Option<(PenTool, Pts)>, strokes: &mut Vec<Pts>, erasers: &mut usize| {
        if let Some((t, s)) = cur.take() {
            if t == PenTool::Pen {
                strokes.push(s)
            } else {
                strokes.retain(|st| !erased_by(st, &s));
                *erasers += 1
            }
        }
    };
    for e in &f.events {
        match e.kind {
            PenEventKind::Down => {
                close(&mut cur, &mut strokes, &mut erasers);
                cur = Some((e.tool, vec![(e.x, e.y, e.p)]));
            }
            PenEventKind::Motion => {
                if let Some((_, s)) = cur.as_mut() {
                    s.push((e.x, e.y, e.p))
                }
            }
            PenEventKind::Up | PenEventKind::ProximityOut => close(&mut cur, &mut strokes, &mut erasers),
            _ => {}
        }
    }
    close(&mut cur, &mut strokes, &mut erasers);
    (strokes, erasers)
}

#[test]
fn every_fixture_replays_exactly_and_deterministically() {
    for path in fixtures() {
        let f = Fixture::read(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let (e1, res) = replay(&f, 64);
        let (e2, _) = replay(&f, 7); // different batching must not change the result
        assert_eq!(
            e1.inspect(None).unwrap(),
            e2.inspect(None).unwrap(),
            "{name}: replay not deterministic"
        );

        let (exp, erasers) = expected(&f);
        let canvas = e1.inspect(Some("/ws/canvases")).unwrap();
        // JSON object keys sort as strings ("10" < "2"): order by numeric id = commit order.
        let mut objs: Vec<&Value> = canvas
            .as_object()
            .unwrap()
            .values()
            .flat_map(|c| c["objects"].as_object().unwrap().values())
            .collect();
        objs.sort_by_key(|o| o["id"].as_u64().unwrap());
        assert_eq!(objs.len(), exp.len(), "{name}: stroke count");
        for (o, ex) in objs.iter().zip(&exp) {
            let samples = o["samples"].as_array().unwrap();
            assert_eq!(samples.len(), ex.len(), "{name}: sample count");
            for (s, &(x, y, p)) in samples.iter().zip(ex) {
                // The model snaps samples to the storage grid (ADR 0005); compare to that.
                let q = vibenote_model::Sample {
                    x,
                    y,
                    pressure: p,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: 0,
                }
                .quantised();
                let (x, y, p) = (&q.x, &q.y, &q.pressure);
                assert_eq!(s["x"].as_f64().unwrap(), *x, "{name}: x");
                assert_eq!(s["y"].as_f64().unwrap(), *y, "{name}: y");
                assert_eq!(s["pressure"].as_f64().unwrap() as f32, *p, "{name}: pressure");
            }
        }
        let reported: usize = res.iter().map(|r| r["errors"].as_array().unwrap().len()).sum();
        assert_eq!(
            reported, 0,
            "{name}: {erasers} eraser strokes must erase without errors"
        );
        // nothing left live after the fixture ends (proximity out)
        assert_eq!(e1.inspect(Some("/live/stroke")).unwrap(), Value::Null, "{name}");
    }
}

#[test]
fn pen_down_without_note_is_reported_not_fatal() {
    let f = Fixture::read(std::io::BufReader::new(std::fs::File::open(&fixtures()[0]).unwrap())).unwrap();
    let mut e = Engine::new().unwrap();
    let r = e.execute("input.pen", json!({"events": f.events})).unwrap();
    assert!(!r["errors"].as_array().unwrap().is_empty());
}

#[test]
fn hostile_event_values_are_rejected() {
    let mut e = Engine::new().unwrap();
    let bad = json!({"events": [{"t_us": 0, "kind": "down", "tool": "pen", "x": f64::MAX, "y": 0.0}]});
    assert!(e.execute("input.pen", bad).is_err());
}
