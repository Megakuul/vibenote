#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]
//! M1.1: tablet-v2 event translation. Every committed ink fixture survives
//! fixture → protocol frames → translator unchanged (R-PEN-004: no sample dropped or altered).

use vibenote_ink::input::{Fixture, PenEvent, PenEventKind, PenTool};
use vibenote_ui::tablet::{ToolEvent, ToolTranslator, protocol_frames};

fn fixtures() -> Vec<(String, Fixture)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ink");
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| {
            let f = Fixture::read(std::io::BufReader::new(std::fs::File::open(&p).unwrap())).unwrap();
            (p.file_name().unwrap().to_string_lossy().into_owned(), f)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(v.len() >= 11, "fixtures missing");
    v
}

fn step(p: f32) -> i64 {
    (f64::from(p) * 65535.0).round() as i64
}

fn translate(events: &[PenEvent]) -> Vec<PenEvent> {
    let mut tr = ToolTranslator::new(events.first().map_or(PenTool::Pen, |e| e.tool));
    let mut out = Vec::new();
    for (t, frame) in protocol_frames(events) {
        for ev in frame {
            out.extend(tr.feed(ev, t));
        }
    }
    out
}

#[test]
fn every_fixture_round_trips_exactly() {
    for (name, f) in fixtures() {
        // Fixtures switch tools (eraser flip) only across proximity; translate per proximity span.
        let mut got = Vec::new();
        let mut span = Vec::new();
        for e in &f.events {
            span.push(*e);
            if e.kind == PenEventKind::ProximityOut {
                got.extend(translate(&span));
                span.clear();
            }
        }
        got.extend(translate(&span));
        assert_eq!(got.len(), f.events.len(), "{name}: event count");
        for (i, (a, b)) in got.iter().zip(&f.events).enumerate() {
            // The generator writes hover distance as plain fractions (e.g. 0.875); the protocol
            // carries 0..=65535, so distance can only match within one protocol step. Distance is
            // hover-only and not stored in strokes. Everything else must be exact.
            // Pressure: the generator writes libinput's float; the protocol (and the model's
            // sample grid, `vibenote_model::grid`) is 1/65535, so it must match on that grid.
            assert!((a.d - b.d).abs() <= 1.0 / 65535.0, "{name}: event {i} distance");
            assert_eq!(step(a.p), step(b.p), "{name}: event {i} pressure");
            let norm = |e: &PenEvent| PenEvent { d: 0.0, p: 0.0, ..*e };
            assert_eq!(norm(a), norm(b), "{name}: event {i}");
        }
    }
}

#[test]
fn pressure_only_update_is_a_sample() {
    let mut tr = ToolTranslator::default();
    tr.feed(ToolEvent::ProximityIn, 0);
    tr.feed(ToolEvent::Motion { x: 10.0, y: 20.0 }, 0);
    tr.feed(ToolEvent::Frame { time_ms: 1 }, 0);
    tr.feed(ToolEvent::Down, 1);
    tr.feed(ToolEvent::Pressure(30000), 1);
    assert_eq!(tr.feed(ToolEvent::Frame { time_ms: 2 }, 1)[0].kind, PenEventKind::Down);
    tr.feed(ToolEvent::Pressure(40000), 2);
    let out = tr.feed(ToolEvent::Frame { time_ms: 3 }, 2);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, PenEventKind::Motion);
    assert_eq!(out[0].p, 40000.0 / 65535.0);
    assert_eq!((out[0].x, out[0].y), (10.0, 20.0));
}

#[test]
fn proximity_out_or_removal_while_touching_ends_the_stroke() {
    for last in [ToolEvent::ProximityOut, ToolEvent::Removed] {
        let mut tr = ToolTranslator::default();
        for e in [ToolEvent::ProximityIn, ToolEvent::Down, ToolEvent::Pressure(1000)] {
            tr.feed(e, 0);
        }
        tr.feed(ToolEvent::Frame { time_ms: 0 }, 0);
        assert!(tr.in_contact());
        let mut out = tr.feed(last, 5);
        if out.is_empty() {
            out = tr.feed(ToolEvent::Frame { time_ms: 5 }, 5);
        }
        let kinds: Vec<_> = out.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [PenEventKind::Up, PenEventKind::ProximityOut], "{last:?}");
        assert!(!tr.in_contact());
    }
}

#[test]
fn hostile_values_are_bounded() {
    let mut tr = ToolTranslator::default();
    for e in [
        ToolEvent::ProximityIn,
        ToolEvent::Motion { x: f64::NAN, y: 1.0 },
        ToolEvent::Pressure(u32::MAX),
        ToolEvent::Distance(u32::MAX),
        ToolEvent::Tilt {
            x: f64::INFINITY,
            y: -1.0e9,
        },
    ] {
        tr.feed(e, 0);
    }
    for i in 0..10_000u32 {
        tr.feed(
            ToolEvent::Button {
                button: i,
                pressed: true,
            },
            0,
        );
    }
    let out = tr.feed(ToolEvent::Frame { time_ms: 0 }, 0);
    assert!(out.len() <= 40);
    for e in &out {
        e.validate().unwrap();
    }
    assert_eq!(out[0].p, 1.0);
    assert_eq!((out[0].tx, out[0].ty), (0.0, -90.0));
}

#[test]
fn double_down_and_stray_up_are_ignored() {
    let mut tr = ToolTranslator::default();
    tr.feed(ToolEvent::Up, 0);
    assert!(tr.feed(ToolEvent::Frame { time_ms: 0 }, 0).is_empty());
    tr.feed(ToolEvent::Down, 1);
    assert_eq!(tr.feed(ToolEvent::Frame { time_ms: 1 }, 1).len(), 1);
    tr.feed(ToolEvent::Down, 2);
    assert!(tr.feed(ToolEvent::Frame { time_ms: 2 }, 2).is_empty());
}
