#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
//! Audio in the engine (M10, ADR 0009): recordings (simulated and live test tone; never the
//! microphone), crash recovery, stroke ↔ audio links through undo and erasing, import, GC.

use serde_json::{Value, json};
use vibenote_engine::Engine;

fn note(e: &mut Engine) -> u64 {
    let n = e.execute("workspace.note.create", json!({"name": "a"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    n
}

fn audio_objects(e: &Engine) -> Vec<(u64, u64, usize)> {
    e.session
        .ws
        .canvases
        .values()
        .flat_map(|c| c.objects.values())
        .filter_map(|o| match &o.kind {
            vibenote_model::ObjectKind::Audio { blobs, duration_ms, .. } => Some((o.id.0, *duration_ms, blobs.len())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_live_recording_runs_while_inking_and_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("w.sqlite");
    let id;
    {
        let mut e = Engine::open(&db).unwrap();
        note(&mut e);
        let t = std::time::Instant::now();
        id = e.execute("audio.record.start", json!({"source": "test-tone"})).unwrap()["id"].clone();
        for k in 0..20 {
            e.execute(
                "ink.stroke.add",
                json!({"samples": [{"x": f64::from(k), "y": 0.0}, {"x": f64::from(k), "y": 9.0}]}),
            )
            .unwrap();
            e.pump(4);
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        let r = e.execute("audio.record.stop", json!({ "id": id })).unwrap();
        let wall = t.elapsed().as_millis() as i64;
        let got = r["duration_ms"].as_i64().unwrap();
        assert!((got - wall).abs() < 300, "recorded {got} ms in {wall} ms");
        // Strokes written meanwhile are linked to it.
        let n = e.session.current_note.unwrap();
        assert_eq!(e.session.ws.audio_marks[&n].len(), 20);
        let p = e
            .execute("audio.play", json!({"id": id, "position_ms": 500, "dry_run": true}))
            .unwrap();
        assert_eq!(p["position_ms"], 500);
    }
    let mut e = Engine::open(&db).unwrap();
    e.load_all().unwrap();
    let objs = audio_objects(&e);
    assert_eq!(objs.len(), 1);
    assert_eq!(json!(objs[0].0), id);
    let decoded = e.session.audio_pcm(vibenote_model::ObjectId(objs[0].0)).unwrap();
    assert!((decoded.duration_ms() as i64 - objs[0].1 as i64).abs() <= 25);
}

#[test]
fn a_recording_interrupted_by_a_crash_is_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("w.sqlite");
    {
        let mut e = Engine::open(&db).unwrap();
        note(&mut e);
        // 25 s: two full chunks are stored and journalled; the last 5 s are not.
        e.execute(
            "audio.record.start",
            json!({"source": "test-tone", "simulate_seconds": 25.0}),
        )
        .unwrap();
        e.sync().unwrap();
        // Dropped without stopping (a crash keeps no more than the journal).
    }
    let mut e = Engine::open(&db).unwrap();
    e.load_all().unwrap();
    let objs = audio_objects(&e);
    assert_eq!(objs.len(), 1, "recovered");
    assert_eq!(objs[0].2, 2, "the stored chunks");
    assert!((objs[0].1 as i64 - 20_000).abs() <= 50, "{} ms", objs[0].1);
    assert!(
        e.session.warnings.iter().any(|w| w.contains("recovered")),
        "{:?}",
        e.session.warnings
    );
    // New ids never collide with the recovered one.
    let n = e.execute("workspace.note.create", json!({"name": "b"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    assert!(n > objs[0].0);
    drop(e);
    // Recovered once only.
    let mut e = Engine::open(&db).unwrap();
    e.load_all().unwrap();
    assert_eq!(audio_objects(&e).len(), 1);
}

#[test]
fn stroke_links_follow_undo_and_erasing() {
    let mut e = Engine::new().unwrap();
    let n = note(&mut e);
    let rec = e
        .execute("audio.record.start", json!({"source": "test-tone", "at_us": 1_000_000}))
        .unwrap()["id"]
        .clone();
    let s = e
        .execute(
            "ink.stroke.add",
            json!({"samples": [{"x": 0.0, "y": 0.0}, {"x": 100.0, "y": 0.0}], "written_at_us": 7_500_000}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    // Before the recording began: no link.
    let early = e
        .execute(
            "ink.stroke.add",
            json!({"samples": [{"x": 0.0, "y": 50.0}], "written_at_us": 500_000}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("audio.record.stop", json!({"id": rec, "at_us": 10_000_000}))
        .unwrap();
    let play =
        |e: &mut Engine, stroke: u64| e.execute("audio.play_at_stroke", json!({"stroke": stroke, "dry_run": true}));
    assert_eq!(play(&mut e, s).unwrap()["position_ms"], 4_500);
    assert!(play(&mut e, early).is_err());
    let lead = e
        .execute(
            "audio.play_at_stroke",
            json!({"stroke": s, "dry_run": true, "lead_in_ms": 0}),
        )
        .unwrap();
    assert_eq!(lead["position_ms"], 6_500);
    // Splitting the stroke: both pieces keep its moment.
    let r = e
        .execute("ink.erase_area", json!({"path": [[50.0, 0.0]], "radius": 3.0}))
        .unwrap();
    let pieces: Vec<u64> = r["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(pieces.len(), 2, "{r}");
    for p in &pieces {
        assert_eq!(play(&mut e, *p).unwrap()["position_ms"], 4_500);
    }
    // Undo of the erase brings back the stroke and its link; undo of the recording removes
    // the object; redo restores it.
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(play(&mut e, s).unwrap()["position_ms"], 4_500);
    let nn = vibenote_model::NoteId(n);
    assert!(
        pieces
            .iter()
            .all(|p| !e.session.ws.audio_marks[&nn].contains_key(&vibenote_model::ObjectId(*p)))
    );
    e.execute("edit.undo", json!({})).unwrap(); // the recording
    assert!(audio_objects(&e).is_empty());
    e.execute("edit.redo", json!({})).unwrap();
    assert_eq!(audio_objects(&e).len(), 1);
}

/// A 16-bit mono WAV file of `ms` milliseconds at 22,050 Hz.
fn wav(ms: u32) -> Vec<u8> {
    let rate = 22_050u32;
    let n = rate * ms / 1000;
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(36 + n * 2).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(n * 2).to_le_bytes());
    for k in 0..n {
        v.extend_from_slice(&(((k % 50) as i16 - 25) * 300).to_le_bytes());
    }
    v
}

#[test]
fn imported_files_are_kept_unchanged_and_play() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("w.sqlite");
    let path = dir.path().join("a.wav");
    let bytes = wav(1500);
    std::fs::write(&path, &bytes).unwrap();
    let mut e = Engine::open(&db).unwrap();
    note(&mut e);
    let r = e
        .execute("audio.import", json!({"path": path, "at": [10.0, 20.0]}))
        .unwrap();
    assert_eq!(r["duration_ms"], 1500);
    assert_eq!((r["rate"].as_u64(), r["channels"].as_u64()), (Some(22_050), Some(1)));
    let blob = r["blob"].as_str().unwrap().to_string();
    let p = e
        .execute(
            "audio.play",
            json!({"id": r["id"], "position_ms": 99_999, "dry_run": true}),
        )
        .unwrap();
    assert_eq!(p["position_ms"], 1500, "clamped to the end");
    // GC keeps it; the stored file is byte-identical.
    e.execute("blob.gc", json!({})).unwrap();
    assert_eq!(&*e.session.blobs.0.get(&blob).unwrap(), &bytes[..]);
    // Hostile files change nothing.
    let before = e.session.ws.clone();
    for junk in [&b"RIFF"[..], b"", &bytes[..44], &[0x42u8; 3000][..]] {
        std::fs::write(&path, junk).unwrap();
        assert!(e.execute("audio.import", json!({"path": path})).is_err());
    }
    assert_eq!(e.session.ws, before);
}

#[test]
fn chunks_of_a_running_recording_survive_garbage_collection() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open(&dir.path().join("w.sqlite")).unwrap();
    note(&mut e);
    let rec = e
        .execute(
            "audio.record.start",
            json!({"source": "test-tone", "simulate_seconds": 15.0}),
        )
        .unwrap()["id"]
        .clone();
    e.execute("blob.gc", json!({})).unwrap();
    let r = e.execute("audio.record.stop", json!({ "id": rec })).unwrap();
    assert_eq!(r["duration_ms"], 15_000);
    let id = vibenote_model::ObjectId(rec.as_u64().unwrap());
    assert_eq!(e.session.audio_pcm(id).unwrap().duration_ms(), 15_000);
    let st: Value = e.execute("audio.status", json!({})).unwrap();
    assert!(st["recording"].is_null());
}
