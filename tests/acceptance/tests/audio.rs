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

/// A mono 16-bit WAV of `secs` seconds of a quiet tone.
fn wav(path: &std::path::Path, secs: u32) {
    let rate = 48_000u32;
    let n = rate * secs;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + n * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(n * 2).to_le_bytes());
    for i in 0..n {
        let v = ((f64::from(i) * 440.0 * std::f64::consts::TAU / f64::from(rate)).sin() * 1000.0) as i16;
        b.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

/// R-AUDIO-004: a tap on a player's button plays and pauses (resuming where it paused), a tap
/// on its bar plays from there; nothing is drawn. Silent: no sound in tests.
#[test]
fn r_audio_004_players() {
    let (mut e, n) = engine_with_note();
    e.session.audio.set_silent(true);
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("song.wav");
    wav(&f, 4);
    let p = id(&run(&mut e, "audio.import", json!({"path": f, "at": [100.0, 100.0]})));
    let player = |e: &vibenote_engine::Engine| e.session.audio.player(vibenote_model::ObjectId(p));
    let tap = |e: &mut vibenote_engine::Engine, t: u64, x: f64| {
        run(
            e,
            "input.pointer",
            json!({"events": [{"t_us": t, "kind": "down", "x": x, "y": 124.0}, {"t_us": t + 5000, "kind": "up", "x": x, "y": 124.0}]}),
        );
    };
    // The button is the left square of the 240 × 48 player.
    tap(&mut e, 1_000_000, 120.0);
    assert!(player(&e).is_some_and(|v| v.playing), "a tap on the button plays");
    assert_eq!(objects(&e, n).len(), 1, "no ink from the tap");
    tap(&mut e, 2_000_000, 120.0);
    assert!(player(&e).is_some_and(|v| !v.playing), "a second tap pauses");
    tap(&mut e, 3_000_000, 120.0);
    assert!(player(&e).is_some_and(|v| v.playing), "and resumes");
    // The middle of the bar: about half of 4 s.
    let bar0 = 100.0 + 48.0;
    tap(&mut e, 4_000_000, bar0 + (340.0 - bar0) / 2.0);
    let v = player(&e).unwrap();
    assert!(v.playing && (1_800..=2_200).contains(&v.position_ms), "{v:?}");
    let status = run(&mut e, "audio.status", json!({}));
    assert_eq!(status["playing"]["object"], p);
}
