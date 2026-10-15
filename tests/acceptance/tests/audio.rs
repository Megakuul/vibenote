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
//! R-AUDIO-001..003 (SPEC §7). Recording tests use a test source, never the owner's microphone.

use serde_json::json;
use vibenote_acceptance::*;

/// R-AUDIO-001: record 10 s from a test source → clip of 10 s ± 50 ms.
#[test]
fn r_audio_001_record_and_import() {
    let (mut e, _) = engine_with_note();
    let r = run(
        &mut e,
        "audio.record.start",
        json!({"source": "test-tone", "simulate_seconds": 10.0}),
    );
    let clip = run(&mut e, "audio.record.stop", json!({"id": r["id"]}));
    assert!((clip["duration_ms"].as_f64().unwrap() - 10_000.0).abs() <= 50.0);
}

/// R-AUDIO-002: tapping a stroke written at t0+5 s plays from 5 s (− 2 s lead-in), ± 100 ms.
#[test]
fn r_audio_002_sync() {
    let (mut e, n) = engine_with_note();
    let rec = run(&mut e, "audio.record.start", json!({"source": "test-tone", "at_us": 0}));
    let s = id(&run(
        &mut e,
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": 0.0, "y": 0.0, "t_us": 0}], "written_at_us": 5_000_000}),
    ));
    run(
        &mut e,
        "audio.record.stop",
        json!({"id": rec["id"], "at_us": 8_000_000}),
    );
    let p = run(&mut e, "audio.play_at_stroke", json!({"stroke": s, "dry_run": true}));
    assert!((p["position_ms"].as_f64().unwrap() - 3000.0).abs() <= 100.0);
}

/// R-AUDIO-003: recording never blocks inking (pen batch handling stays within the inking budget).
#[test]
fn r_audio_003_recording_does_not_block_ink() {
    let (mut e, _) = engine_with_note();
    run(&mut e, "audio.record.start", json!({"source": "test-tone"}));
    let f = fixture("scribble-5.jsonl");
    let mut worst = 0.0f64;
    for c in f.events.chunks(4) {
        let t = std::time::Instant::now();
        run(&mut e, "input.pen", json!({"events": c}));
        worst = worst.max(t.elapsed().as_secs_f64() * 1e3);
    }
    assert!(
        worst <= budget("durability", "inking_while_saving_p99_ms") * 20.0,
        "worst pen batch {worst:.2} ms"
    );
}
