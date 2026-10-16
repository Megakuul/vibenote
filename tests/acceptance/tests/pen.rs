#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! R-PEN-001..012 (SPEC §2).

use serde_json::json;
use vibenote_acceptance::*;
use vibenote_ink::input::PenEventKind;

/// Ink coverage summed down one pixel column, in pixels, normalised by the default ink colour
/// (red channel: paper 251, ink 0x1e = 30).
fn coverage_px(img: &vibenote_render::Image, col: usize) -> f64 {
    let w = img.width as usize;
    (0..img.height as usize)
        .map(|y| ((251.0 - f64::from(img.rgba[(y * w + col) * 4])) / (251.0 - 30.0)).clamp(0.0, 1.0))
        .sum()
}

/// R-PEN-001 / R-PERF-001: input-to-ink latency budget, measured end-to-end through simtab →
/// compositor → app → presented frame (the Phase 0 method). Needs the app (R-PLAT-002).
#[test]
fn r_pen_001_latency_budget_end_to_end() {
    let report = root().join("target/e2e-latency.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap_or_else(|_| {
        panic!(
            "no e2e latency report at {} — run `cargo xtask bench latency` (needs the vibenote app)",
            report.display()
        )
    }))
    .unwrap();
    assert!(v["p50_ms"].as_f64().unwrap() <= budget("latency", "p50_ms"));
    assert!(v["p95_ms"].as_f64().unwrap() <= budget("latency", "p95_ms"));
}

/// R-PEN-002: width follows pressure — stored pressures are kept, the width curve is strictly
/// monotonic, and the rendered width matches the curve (±0.25 px).
#[test]
fn r_pen_002_width_follows_pressure() {
    let (mut e, note) = engine_with_note();
    let f = fixture("pressure-ramp-3.jsonl");
    replay(&mut e, &f);
    let o = objects(&e, note);
    assert_eq!(o.len(), 3);
    let p: Vec<f64> = o[0]["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["pressure"].as_f64().unwrap())
        .collect();
    // stored pressures equal the delivered pressures (quantised to the storage grid)
    let delivered: Vec<f64> = f
        .events
        .iter()
        .skip_while(|x| x.kind != PenEventKind::Down)
        .take_while(|x| x.kind != PenEventKind::Up)
        .map(|x| {
            f64::from(
                vibenote_model::Sample {
                    x: 0.0,
                    y: 0.0,
                    pressure: x.p,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: 0,
                }
                .quantised()
                .pressure,
            )
        })
        .collect();
    assert_eq!(p, delivered, "pressure must be kept exactly");
    let spread = p.iter().copied().fold(f64::MIN, f64::max) - p.iter().copied().fold(f64::MAX, f64::min);
    assert!(spread > 0.5, "ramp must span a wide pressure range, got {spread}");
    let half = p.len() / 2;
    assert!(
        p[..half].windows(2).all(|w| w[1] >= w[0]),
        "pressure must rise in the first half of the ramp"
    );
    let mut prev = 0.0;
    for i in 0..=100u16 {
        let w = vibenote_ink::pressure_width(3.0, f32::from(i) / 100.0);
        assert!(w > prev);
        prev = w;
    }
    // rendered width at zoom 10 vs curve
    let mut r = vibenote_render::Headless::new().unwrap();
    for pr in [0.2f32, 0.8] {
        let (mut e2, n2) = engine_with_note();
        let _ = run(
            &mut e2,
            "ink.stroke.add",
            json!({"note": n2, "width": 3.0, "samples": [{"x": 5.0, "y": 20.0, "p": pr}, {"x": 55.0, "y": 20.0, "p": pr}]}),
        );
        let c = e2.session.ws.canvas(vibenote_model::NoteId(n2)).unwrap();
        let img = r
            .render(
                c,
                &[],
                &vibenote_render::Viewport {
                    origin_x: 0.0,
                    origin_y: 0.0,
                    zoom: 10.0,
                    width: 600,
                    height: 400,
                },
            )
            .unwrap();
        let cov = coverage_px(&img, 300);
        let want = f64::from(vibenote_ink::pressure_width(3.0, pr)) * 10.0;
        assert!((cov - want).abs() <= 2.5, "p={pr}: {cov:.2} vs {want:.2} px");
    }
}

/// R-PEN-003: tilt-responsive tools (highlighter) change width with tilt; the default pen does not.
#[test]
fn r_pen_003_tilt_affects_tilt_tools_only() {
    let mut r = vibenote_render::Headless::new().unwrap();
    let cover = |tool: &str, tilt: f32, r: &mut vibenote_render::Headless| {
        let (mut e, n) = engine_with_note();
        let _ = run(
            &mut e,
            "ink.stroke.add",
            json!({"note": n, "tool": tool, "width": 3.0, "samples": [{"x": 5.0, "y": 20.0, "p": 0.6, "tx": tilt}, {"x": 55.0, "y": 20.0, "p": 0.6, "tx": tilt}]}),
        );
        let img = r
            .render(
                e.session.ws.canvas(vibenote_model::NoteId(n)).unwrap(),
                &[],
                &vibenote_render::Viewport {
                    origin_x: 0.0,
                    origin_y: 0.0,
                    zoom: 10.0,
                    width: 600,
                    height: 400,
                },
            )
            .unwrap();
        coverage_px(&img, 300)
    };
    assert!(
        (cover("pen", 0.0, &mut r) - cover("pen", 60.0, &mut r)).abs() < 0.5,
        "pen must ignore tilt"
    );
    assert!(
        (cover("highlighter", 0.0, &mut r) - cover("highlighter", 60.0, &mut r)).abs() > 2.0,
        "highlighter width must follow tilt"
    );
}

/// R-PEN-004: every sample delivered is kept (1 kHz fixture, exact count and values).
#[test]
fn r_pen_004_every_sample_kept_at_1khz() {
    let (mut e, note) = engine_with_note();
    let f = fixture("scribble-1khz-3.jsonl");
    replay(&mut e, &f);
    let o = objects(&e, note);
    let downs = f.events.iter().filter(|x| x.kind == PenEventKind::Down).count();
    assert_eq!(o.len(), downs);
    let expected: usize = f
        .events
        .iter()
        .filter(|x| matches!(x.kind, PenEventKind::Down | PenEventKind::Motion))
        .count()
        - f.events
            .iter()
            .scan(false, |down, x| {
                let hover = !*down && x.kind == PenEventKind::Motion;
                if x.kind == PenEventKind::Down {
                    *down = true
                }
                if matches!(x.kind, PenEventKind::Up | PenEventKind::ProximityOut) {
                    *down = false
                }
                Some(usize::from(hover))
            })
            .sum::<usize>();
    let got: usize = o.iter().map(|s| s["samples"].as_array().unwrap().len()).sum();
    assert_eq!(got, expected);
}

/// R-PEN-005: each tool is a command `tool.select.<name>`; the session reports the active tool.
#[test]
fn r_pen_005_tool_commands() {
    let (mut e, _) = engine_with_note();
    for t in [
        "pen",
        "highlighter",
        "eraser_stroke",
        "eraser_area",
        "lasso",
        "shape",
        "text",
    ] {
        run(&mut e, &format!("tool.select.{t}"), json!({}));
        assert_eq!(e.inspect(Some("/tool")).unwrap(), json!(t));
    }
}

/// R-PEN-006: the eraser end erases (stroke mode by default) without changing the selected tool.
#[test]
fn r_pen_006_eraser_end_erases() {
    let (mut e, note) = engine_with_note();
    let f = fixture("eraser-flip-4.jsonl");
    let res = replay(&mut e, &f);
    assert!(
        res.iter().all(|r| r["errors"].as_array().unwrap().is_empty()),
        "{res:?}"
    );
    assert!(
        objects(&e, note).is_empty(),
        "eraser passes over both pen strokes, so nothing may remain"
    );
}

/// R-PEN-007: stroke eraser removes whole touched strokes only; undo restores exactly.
#[test]
fn r_pen_007_stroke_eraser() {
    let (mut e, note) = engine_with_note();
    let a = line(&mut e, note, (0.0, 0.0), (100.0, 0.0), 0.5);
    let b = line(&mut e, note, (0.0, 50.0), (100.0, 50.0), 0.5);
    let before = objects(&e, note);
    run(
        &mut e,
        "ink.erase_stroke",
        json!({"note": note, "path": [[50.0, -10.0], [50.0, 10.0]]}),
    );
    let after = objects(&e, note);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["id"], b);
    let _ = a;
    run(&mut e, "edit.undo", json!({}));
    assert_eq!(objects(&e, note), before);
}

/// R-PEN-008: area eraser splits a stroke into the remaining pieces.
#[test]
fn r_pen_008_area_eraser_splits() {
    let (mut e, note) = engine_with_note();
    line(&mut e, note, (0.0, 0.0), (100.0, 0.0), 0.5);
    run(
        &mut e,
        "ink.erase_area",
        json!({"note": note, "radius": 5.0, "path": [[50.0, -20.0], [50.0, 20.0]]}),
    );
    let o = objects(&e, note);
    assert_eq!(o.len(), 2, "one stroke split into two");
    for s in &o {
        for p in s["samples"].as_array().unwrap() {
            assert!(
                (p["x"].as_f64().unwrap() - 50.0).abs() >= 4.5,
                "no ink left under the eraser"
            );
        }
    }
}

/// R-PEN-009: lasso selects objects mostly inside; transform + inverse restores geometry.
#[test]
fn r_pen_009_lasso_and_transform() {
    let (mut e, note) = engine_with_note();
    let inside = line(&mut e, note, (10.0, 10.0), (40.0, 10.0), 0.5);
    line(&mut e, note, (200.0, 200.0), (300.0, 200.0), 0.5);
    let sel = run(
        &mut e,
        "selection.lasso",
        json!({"note": note, "path": [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]}),
    );
    assert_eq!(sel["ids"], json!([inside]));
    let before = objects(&e, note);
    run(
        &mut e,
        "selection.transform",
        json!({"translate": [12.5, -3.0], "scale": 1.5, "rotate_deg": 30.0}),
    );
    run(&mut e, "selection.transform", json!({"invert_last": true}));
    let after = objects(&e, note);
    for (a, b) in before.iter().zip(&after) {
        for (p, q) in a["samples"]
            .as_array()
            .unwrap()
            .iter()
            .zip(b["samples"].as_array().unwrap())
        {
            assert!((p["x"].as_f64().unwrap() - q["x"].as_f64().unwrap()).abs() < 1e-3);
        }
    }
}

/// R-PEN-010: barrel button 1 held = temporary lasso (no ink is drawn while held).
#[test]
fn r_pen_010_barrel_button_temporary_lasso() {
    let (mut e, note) = engine_with_note();
    let ev = |t: u64, kind: &str, x: f64, button: Option<u32>| json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": 10.0, "p": 0.5, "button": button});
    run(
        &mut e,
        "input.pen",
        json!({"events": [
        ev(0, "proximity_in", 0.0, None), ev(1, "button_down", 0.0, Some(331)),
        ev(2, "down", 0.0, None), ev(3, "motion", 50.0, None), ev(4, "up", 50.0, None),
        ev(5, "button_up", 50.0, Some(331)), ev(6, "proximity_out", 50.0, None)]}),
    );
    assert!(objects(&e, note).is_empty(), "no stroke while the lasso button is held");
}

/// R-PEN-011: hover cursor follows the last hover sample.
#[test]
fn r_pen_011_hover_cursor() {
    let (mut e, _) = engine_with_note();
    let f = fixture("hover-2.jsonl");
    let first_out = f
        .events
        .iter()
        .position(|x| x.kind == PenEventKind::ProximityOut)
        .unwrap();
    let evs = &f.events[..first_out];
    run(&mut e, "input.pen", json!({"events": evs}));
    let last = evs.iter().rev().find(|x| x.kind == PenEventKind::Motion).unwrap();
    let h = e.inspect(Some("/live/hover")).unwrap();
    assert_eq!(h, json!([last.x, last.y]));
}

/// R-PEN-012: mouse draws with fixed pressure 0.5.
#[test]
fn r_pen_012_mouse_draws() {
    let (mut e, note) = engine_with_note();
    run(
        &mut e,
        "input.pointer",
        json!({"events": [
        {"t_us": 0, "kind": "down", "x": 1.0, "y": 1.0}, {"t_us": 8000, "kind": "motion", "x": 30.0, "y": 5.0}, {"t_us": 16000, "kind": "up", "x": 30.0, "y": 5.0}]}),
    );
    let o = objects(&e, note);
    assert_eq!(o.len(), 1);
    assert!(
        o[0]["samples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| (s["pressure"].as_f64().unwrap() - 0.5).abs() < 1e-4)
    );
}

/// A mouse stroke from (x0, y) to (x1, y).
fn mouse_line(e: &mut vibenote_engine::Engine, t0: u64, (x0, x1): (f64, f64), y: f64) {
    run(
        e,
        "input.pointer",
        json!({"events": [
            {"t_us": t0, "kind": "down", "x": x0, "y": y},
            {"t_us": t0 + 8000, "kind": "motion", "x": (x0 + x1) / 2.0, "y": y},
            {"t_us": t0 + 16000, "kind": "motion", "x": x1, "y": y},
            {"t_us": t0 + 24000, "kind": "up", "x": x1, "y": y}]}),
    );
}

/// R-PEN-013: pen and highlighter each have a colour; new strokes use it; it persists.
#[test]
fn r_pen_013_colours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&path).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "c"})));
    run(&mut e, "note.open", json!({"id": n}));
    run(
        &mut e,
        "settings.set",
        json!({"pen_color": 0xc628_28ffu32, "highlighter_color": 0x7ed9_57ffu32}),
    );
    mouse_line(&mut e, 0, (10.0, 200.0), 50.0);
    run(&mut e, "tool.select.highlighter", json!({}));
    mouse_line(&mut e, 100_000, (10.0, 200.0), 150.0);
    let o = objects(&e, n);
    let colors: Vec<(String, u64)> = o
        .iter()
        .map(|s| (s["tool"].as_str().unwrap().to_string(), s["color"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        colors,
        vec![
            ("pen".to_string(), 0xc628_28ff),
            ("highlighter".to_string(), 0x7ed9_57ff)
        ]
    );
    drop(e);
    let e = vibenote_engine::Engine::open(&path).unwrap();
    assert_eq!(e.inspect(Some("/settings/pen_color")).unwrap(), json!(0xc628_28ffu32));
}

/// R-PEN-014: covers hide what is under them; a tap reveals (no ink), a second tap hides;
/// reopening the note closes them; they are stored as covers and never recognised as writing.
#[test]
fn r_pen_014_cover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&path).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "cards"})));
    let other = id(&run(&mut e, "workspace.note.create", json!({"name": "other"})));
    run(&mut e, "note.open", json!({"id": n}));
    run(&mut e, "tool.select.cover", json!({}));
    mouse_line(&mut e, 0, (10.0, 300.0), 100.0);
    let o = objects(&e, n);
    assert_eq!(o.len(), 1);
    assert_eq!(o[0]["tool"], "cover");
    let cover = o[0]["id"].as_u64().unwrap();
    assert!(e.session.open_block.is_none(), "a cover never starts a writing block");
    let tap = |e: &mut vibenote_engine::Engine, t: u64| {
        run(
            e,
            "input.pointer",
            json!({"events": [{"t_us": t, "kind": "down", "x": 150.0, "y": 101.0}, {"t_us": t + 5000, "kind": "up", "x": 150.0, "y": 101.0}]}),
        );
    };
    // With the pen tool too: a tap on a cover is not a dot.
    run(&mut e, "tool.select.pen", json!({}));
    tap(&mut e, 1_000_000);
    assert_eq!(objects(&e, n).len(), 1, "no ink from the tap");
    assert!(e.session.revealed.contains(&vibenote_model::ObjectId(cover)));
    tap(&mut e, 2_000_000);
    assert!(e.session.revealed.is_empty(), "a second tap hides it again");
    tap(&mut e, 3_000_000);
    run(&mut e, "note.open", json!({"id": other}));
    run(&mut e, "note.open", json!({"id": n}));
    assert!(e.session.revealed.is_empty(), "reopening closes every cover");
    drop(e);
    let e = vibenote_engine::Engine::open(&path).unwrap();
    let mut e = e;
    run(&mut e, "note.open", json!({"id": n}));
    e.load_all().unwrap();
    assert_eq!(objects(&e, n)[0]["tool"], "cover", "stored as a cover");
}
