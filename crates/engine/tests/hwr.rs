#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]
//! Handwriting in the engine (M9): written blocks are recognised in the background and become
//! searchable; jobs can be cancelled; the classifier tells writing from drawing; packs load.

use serde_json::{Value, json};
use vibenote_engine::Engine;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn engine_with_ink(fixture: &str) -> (Engine, Vec<u64>) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "hw"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    let text = std::fs::read_to_string(root().join("tests/fixtures/ink").join(fixture)).unwrap();
    let events: Vec<Value> = text.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect();
    let r = e.execute("input.pen", json!({ "events": events })).unwrap();
    let ids = r["committed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    (e, ids)
}

#[test]
fn written_blocks_become_searchable_without_asking() {
    // What the recogniser reads in this ink (explicitly, in a separate engine).
    let (mut probe, ids) = engine_with_ink("loops-4.jsonl");
    let read = probe.execute("hwr.recognize_block", json!({"strokes": ids})).unwrap();
    let text = read["text"].as_str().unwrap().to_string();
    let word: String = text.split_whitespace().next().unwrap_or("").chars().take(3).collect();
    assert!(!word.is_empty(), "the recogniser read nothing: {read}");
    // The same ink, just written: a search finds it as ink.
    let (mut e, ids) = engine_with_ink("loops-4.jsonl");
    let hits = e.execute("search.query", json!({ "text": word })).unwrap();
    let ink: Vec<&Value> = hits["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["source"] == "ink")
        .collect();
    assert_eq!(ink.len(), 1, "{hits}");
    assert_eq!(ink[0]["object"], json!(ids[0]));
    let ink_hits = |e: &mut Engine| {
        let hits = e.execute("search.query", json!({ "text": word })).unwrap();
        hits["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h["source"] == "ink")
            .count()
    };
    // The text belongs to the block: deleting (or moving) its first stroke keeps it
    // (review M6–M9: it used to vanish and never come back).
    e.execute("object.delete", json!({"ids": [ids[0]]})).unwrap();
    assert_eq!(ink_hits(&mut e), 1);
    // With none of its strokes left it leaves search; undo brings the strokes back and the
    // block is read again.
    e.execute("object.delete", json!({"ids": ids[1..]})).unwrap();
    assert_eq!(ink_hits(&mut e), 0);
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(ink_hits(&mut e), 1);
}

#[test]
fn background_jobs_finish_or_cancel() {
    let (mut e, ids) = engine_with_ink("loops-4.jsonl");
    let job = e
        .execute("hwr.recognize_block", json!({"strokes": ids, "background": true}))
        .unwrap()["job"]
        .clone();
    let t = std::time::Instant::now();
    loop {
        let st = e.execute("jobs.status", json!({ "id": job })).unwrap();
        if st["state"] == "done" {
            assert!(st["result"]["text"].is_string());
            break;
        }
        assert!(t.elapsed().as_secs() < 30, "{st}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        e.execute("jobs.cancel", json!({ "id": job })).unwrap()["cancelled"],
        json!(false),
        "already done"
    );
    assert!(e.execute("jobs.status", json!({"id": 999_999})).is_err());
    // Many queued jobs: cancelling one that has not run yet stops it.
    let jobs: Vec<Value> = (0..20)
        .map(|_| {
            e.execute("hwr.recognize_block", json!({"strokes": ids, "background": true}))
                .unwrap()["job"]
                .clone()
        })
        .collect();
    let last = jobs.last().unwrap();
    assert_eq!(
        e.execute("jobs.cancel", json!({ "id": last })).unwrap()["cancelled"],
        json!(true)
    );
    assert_eq!(
        e.execute("jobs.status", json!({ "id": last })).unwrap()["state"],
        json!("cancelled")
    );
}

#[test]
fn the_classifier_tells_writing_from_drawing() {
    let set = std::fs::read_to_string(root().join("tests/eval/writing-drawing/blocks.jsonl")).unwrap();
    let rows: Vec<Value> = set.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "c"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    for label in ["writing", "drawing"] {
        let row = rows
            .iter()
            .find(|r| r["label"] == label && r["source"] != "fhswf-skeleton")
            .unwrap();
        let mut ids = vec![];
        for s in row["strokes"].as_array().unwrap() {
            let samples: Vec<Value> = s
                .as_array()
                .unwrap()
                .iter()
                .map(|p| json!({"x": p[0], "y": p[1], "p": 0.5}))
                .collect();
            ids.push(e.execute("ink.stroke.add", json!({ "samples": samples })).unwrap()["id"].clone());
        }
        let c = e.execute("hwr.classify_block", json!({ "strokes": ids })).unwrap();
        assert_ne!(
            c["class"],
            json!(if label == "writing" { "drawing" } else { "writing" }),
            "{label}: {c}"
        );
    }
}

#[test]
fn packs_are_listed_and_hostile_manifests_rejected() {
    let mut e = Engine::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad");
    std::fs::create_dir(&bad).unwrap();
    std::fs::write(bad.join("manifest.json"), r#"{"format":"vibenote-language-pack","version":1,"lang":"zz","name":"x","model":"../../etc/passwd","dict":"d.txt"}"#).unwrap();
    let junk = dir.path().join("junk");
    std::fs::create_dir(&junk).unwrap();
    std::fs::write(junk.join("manifest.json"), "not json").unwrap();
    let r = e
        .execute(
            "hwr.packs",
            json!({"dirs": [root().join("tests/eval/packs"), dir.path()]}),
        )
        .unwrap();
    let langs: Vec<&str> = r["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["lang"].as_str().unwrap())
        .collect();
    assert!(
        langs.contains(&"de+en") && langs.contains(&"xx") && langs.contains(&"zz"),
        "{r}"
    );
    assert_eq!(
        r["errors"].as_array().unwrap().len(),
        1,
        "the junk manifest is an error"
    );
    // The bad pack's model path is refused when it is used.
    let pack = vibenote_hwr::pack::load(&bad).unwrap();
    assert!(pack.model_path().is_err());
}

/// One pen stroke as events (proximity in, down, motion, up, out) from `t0` (µs).
fn stroke_events(pts: &[(f64, f64)], t0: u64) -> Vec<Value> {
    let ev = |t: u64, kind: &str, (x, y): (f64, f64), p: f64| json!({"t_us": t, "kind": kind, "tool": "pen", "x": x, "y": y, "p": p, "tx": 0.0, "ty": 0.0, "d": 0.0});
    let mut v = vec![ev(t0, "proximity_in", pts[0], 0.0), ev(t0, "down", pts[0], 0.5)];
    for (k, p) in pts.iter().enumerate() {
        v.push(ev(t0 + 5_000 * k as u64, "motion", *p, 0.5));
    }
    let end = t0 + 5_000 * pts.len() as u64;
    v.push(ev(end, "up", *pts.last().unwrap(), 0.0));
    v.push(ev(end, "proximity_out", *pts.last().unwrap(), 0.0));
    v
}

/// Regression (M6–M9 review): closing a block rasterised its lines on the input thread
/// (1.1 s for 60 dense shading strokes in a debug build); now the worker does it.
#[test]
fn closing_a_large_block_does_not_rasterise_on_the_pen_path() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "n"})).unwrap()["id"].clone();
    e.execute("note.open", json!({"id": n})).unwrap();
    let mut t = 0u64;
    for k in 0..60 {
        let pts: Vec<(f64, f64)> = (0..400)
            .map(|i| {
                (
                    100.0 + f64::from(i) * 2.0,
                    100.0 + f64::from(k) * 3.0 + if i % 2 == 0 { 0.0 } else { 40.0 },
                )
            })
            .collect();
        let ev = stroke_events(&pts, t);
        e.execute("input.pen", json!({ "events": ev })).unwrap();
        t += 400 * 5_000 + 100_000;
    }
    // Far away: the block closes on this stroke.
    let far: Vec<(f64, f64)> = (0..20).map(|i| (5_000.0 + f64::from(i), 5_000.0)).collect();
    let ev = stroke_events(&far, t);
    let start = std::time::Instant::now();
    e.execute("input.pen", json!({ "events": ev })).unwrap();
    let ms = start.elapsed().as_secs_f64() * 1e3;
    eprintln!("closing stroke: {ms:.1} ms");
    assert!(ms < 150.0, "pen-up that closes a large block took {ms:.1} ms");
}

/// Regression (M6–M9 review): erasers and the lasso hit prettified strokes where they are
/// shown, not where they are stored; split pieces keep the display transform.
#[test]
fn erasers_and_lasso_use_the_displayed_position_of_prettified_strokes() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "n"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    // Six short vertical-ish "letters" along a line tilted by 20°.
    let tilt = 20f64.to_radians();
    let mut ids = Vec::new();
    for k in 0..6 {
        let samples: Vec<Value> = (0..12)
            .map(|i| {
                let (x, y) = (f64::from(k) * 60.0 + f64::from(i % 3) * 4.0, f64::from(i) * 2.0);
                json!({"x": 500.0 + x * tilt.cos() - y * tilt.sin(), "y": 500.0 + x * tilt.sin() + y * tilt.cos(), "p": 0.5})
            })
            .collect();
        let r = e.execute("ink.stroke.add", json!({ "samples": samples })).unwrap();
        ids.push(r["id"].as_u64().unwrap());
    }
    let r = e
        .execute("hwr.prettify_block", json!({"strokes": ids, "force": true}))
        .unwrap();
    assert_eq!(r["applied"], true, "{r}");
    let note = vibenote_model::NoteId(n);
    let last = vibenote_model::ObjectId(*ids.last().unwrap());
    let t = e.session.display_transforms(note)[&last];
    let canvas = e.session.ws.canvas(note).unwrap();
    let vibenote_model::ObjectKind::Stroke(st) = &canvas.objects[&last].kind else {
        panic!()
    };
    let mid = &st.samples[6];
    let stored = (mid.x, mid.y);
    let shown = (t[0] * mid.x + t[2] * mid.y + t[4], t[1] * mid.x + t[3] * mid.y + t[5]);
    assert!(
        (shown.0 - stored.0).hypot(shown.1 - stored.1) > 20.0,
        "{stored:?} {shown:?}"
    );
    // Erasing where it was stored hits nothing; where it is shown hits it.
    let r = e
        .execute(
            "ink.erase_stroke",
            json!({"path": [[stored.0, stored.1]], "radius": 2.0}),
        )
        .unwrap();
    assert!(r["erased"].as_array().unwrap().is_empty(), "{r}");
    let r = e
        .execute("ink.erase_stroke", json!({"path": [[shown.0, shown.1]], "radius": 2.0}))
        .unwrap();
    assert_eq!(r["erased"], json!([last.0]), "{r}");
    e.execute("edit.undo", json!({})).unwrap();
    // The lasso around the shown stroke selects it.
    let (cx, cy) = shown;
    let lasso = json!([
        [cx - 25.0, cy - 25.0],
        [cx + 25.0, cy - 25.0],
        [cx + 25.0, cy + 25.0],
        [cx - 25.0, cy + 25.0]
    ]);
    let r = e.execute("selection.lasso", json!({ "path": lasso })).unwrap();
    assert!(r["ids"].as_array().unwrap().contains(&json!(last.0)), "{r}");
    // Splitting it keeps the pieces where they were shown.
    let r = e
        .execute("ink.erase_area", json!({"path": [[shown.0, shown.1]], "radius": 1.5}))
        .unwrap();
    let created: Vec<u64> = r["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert!(!created.is_empty(), "{r}");
    let d = e.session.display_transforms(note);
    for c in &created {
        assert_eq!(d.get(&vibenote_model::ObjectId(*c)), Some(&t), "piece {c}");
    }
    assert!(!d.contains_key(&last));
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(e.session.display_transforms(note).get(&last), Some(&t));
}

/// Regression (M6–M9 review): blocks open at exit, and blocks read by another pack version,
/// are recognised (again) when their note is opened.
#[test]
fn blocks_are_read_after_a_restart_and_after_a_pack_change() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("w.sqlite");
    let events: Vec<Value> = std::fs::read_to_string(root().join("tests/fixtures/ink/loops-4.jsonl"))
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let n;
    {
        let mut e = Engine::open(&db).unwrap();
        n = e.execute("workspace.note.create", json!({"name": "hw"})).unwrap()["id"].clone();
        e.execute("note.open", json!({"id": n})).unwrap();
        e.execute("input.pen", json!({ "events": events })).unwrap();
        // Dropped with the block still open: no recognition ran.
    }
    let rows = |db: &std::path::Path| -> Vec<(i64, Option<String>)> {
        let c = rusqlite::Connection::open(db).unwrap();
        let mut q = c.prepare("SELECT block, tag FROM ink_block").unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let stored = rows(&db);
    assert_eq!(stored.len(), 1, "the open block was recorded at exit");
    assert_eq!(stored[0].1, None, "not read yet");
    let ink_hits = |e: &mut Engine| {
        // A search waits for recognition and brings storage in sync.
        e.execute("search.query", json!({"text": "zzz"})).unwrap();
        let c = rusqlite::Connection::open(&db).unwrap();
        c.query_row("SELECT count(*) FROM search_row WHERE source = 'ink'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    {
        let mut e = Engine::open(&db).unwrap();
        e.execute("note.open", json!({"id": n})).unwrap();
        e.wait_loaded();
        assert!(ink_hits(&mut e) >= 1, "read after the restart");
    }
    let tag = rows(&db)[0].1.clone().expect("tag recorded");
    // Another pack version read it: it is read again.
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("UPDATE ink_block SET tag = 'old-pack@0'", []).unwrap();
        c.execute("DELETE FROM search_fts", []).unwrap();
        c.execute("DELETE FROM search_row WHERE source = 'ink'", []).unwrap();
    }
    {
        let mut e = Engine::open(&db).unwrap();
        e.execute("note.open", json!({"id": n})).unwrap();
        e.wait_loaded();
        assert!(ink_hits(&mut e) >= 1, "read again");
    }
    assert_eq!(rows(&db)[0].1.as_deref(), Some(tag.as_str()));
}

/// Mode "suggest" (R-HWR-006): a recognised block passing both gates becomes a suggestion,
/// exactly when `hwr.prettify_block` would apply; applying it is one undo step, dismissing it
/// changes nothing.
#[test]
fn suggest_mode_offers_what_prettify_would_apply() {
    for fixture in ["loops-4.jsonl", "scribble-5.jsonl", "lines-5.jsonl"] {
        let (mut e, ids) = engine_with_ink(fixture);
        assert_eq!(
            e.session.settings.prettify_mode,
            vibenote_model::settings::PrettifyMode::Suggest
        );
        // A search finishes the block and its recognition.
        e.execute("search.query", json!({"text": "x"})).unwrap();
        let note = e.session.current_note.unwrap();
        let offered = !e.session.prettify_suggestions(note).is_empty();
        let ws = e.session.ws.clone();
        if offered {
            let block = e.session.prettify_suggestions(note)[0].0;
            e.execute("hwr.suggestion", json!({"block": block})).unwrap();
            assert_eq!(e.session.ws, ws, "dismissing changes nothing");
            assert!(e.session.prettify_suggestions(note).is_empty());
        }
        let r = e.execute("hwr.prettify_block", json!({"strokes": ids})).unwrap();
        assert_eq!(offered, r["applied"] == true, "{fixture}: {r}");
    }
    // Apply: a block offered by hand (the gates are tested above).
    let (mut e, ids) = engine_with_ink("lines-5.jsonl");
    e.execute("search.query", json!({"text": "x"})).unwrap();
    let note = e.session.current_note.unwrap();
    let block = ids[0];
    e.session.suggestions = vec![vibenote_engine::hwr::Suggestion { note, block }];
    assert_eq!(e.session.prettify_suggestions(note).len(), 1);
    let before = e.session.ws.clone();
    e.execute("hwr.suggestion", json!({"block": block, "apply": true}))
        .unwrap();
    assert!(e.session.ws.prettified.get(&note).is_some_and(|m| !m.is_empty()));
    assert!(
        e.session.prettify_suggestions(note).is_empty(),
        "applied blocks are no suggestion"
    );
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(e.session.ws, before);
    assert!(e.execute("hwr.suggestion", json!({"block": block})).is_err(), "gone");
}
