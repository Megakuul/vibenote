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
//! R-PERF-001..010 (SPEC §15), numbers from tests/budgets.toml. R-PERF-001 lives in pen.rs.

use serde_json::json;
use vibenote_acceptance::*;
use vibenote_render::{Headless, Viewport};

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
        / 1024.0
}

/// R-PERF-002: frame time while panning/zooming the reference note, p99 ≤ budget.
#[test]
fn r_perf_002_frame_time_reference_note() {
    let (db, note) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    e.load_all().unwrap();
    let c = e.session.ws.canvas(note).unwrap();
    let mut r = Headless::new().unwrap();
    let mut t = vec![];
    for (i, zoom) in [1.0, 1.0, 0.5, 0.1, 0.037].iter().enumerate() {
        let vp = Viewport {
            origin_x: i as f64 * 37.0,
            origin_y: 0.0,
            zoom: *zoom,
            width: 1920,
            height: 1200,
        };
        let s = std::time::Instant::now();
        r.render(c, &[], &vp).unwrap();
        t.push(s.elapsed().as_secs_f64() * 1e3);
    }
    t.sort_by(|a, b| a.total_cmp(b));
    assert!(t[t.len() - 1] <= budget("frame", "p99_ms"), "frame times {t:?} ms");
}

/// R-PERF-002 with the whole reference note on the GPU: after settling once, frames at every
/// zoom (down to 1 %, with small pans) stay within the frame budget (p99, and at most
/// `missed_per_1000` over budget per 1,000 frames).
#[test]
fn r_perf_002_settled_frames_reference_note() {
    let (db, note) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    e.load_all().unwrap();
    let c = e.session.ws.canvas(note).unwrap();
    let mut r = Headless::new().unwrap();
    let all = Viewport {
        origin_x: 0.0,
        origin_y: 0.0,
        zoom: 0.01,
        width: 1920,
        height: 1200,
    };
    r.render_settled(c, &[], &all, 10_000).unwrap();
    let mut t = vec![];
    for zoom in [1.0, 0.5, 0.25, 0.1, 0.037, 0.02, 0.01] {
        for k in 0..20 {
            let vp = Viewport {
                origin_x: f64::from(k) * 9.0 / zoom,
                origin_y: f64::from(k) * 5.0 / zoom,
                zoom,
                width: 1920,
                height: 1200,
            };
            let s = std::time::Instant::now();
            r.render(c, &[], &vp).unwrap();
            t.push(s.elapsed().as_secs_f64() * 1e3);
        }
    }
    t.sort_by(|a, b| a.total_cmp(b));
    let budget_ms = budget("frame", "p99_ms");
    let p99 = t[(t.len() * 99).div_ceil(100) - 1];
    let over = t.iter().filter(|&&v| v > budget_ms).count();
    let allowed = (budget("frame", "missed_per_1000") * t.len() as f64 / 1000.0).ceil() as usize;
    assert!(
        p99 <= budget_ms && over <= allowed,
        "p99 {p99:.2} ms, {over} frames over budget; times {t:?}"
    );
}

/// R-PERF-003: cold start to first interactive frame (needs the app).
#[test]
fn r_perf_003_cold_start() {
    let report = root().join("target/e2e-startup.json");
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&report)
            .unwrap_or_else(|_| panic!("no startup report ({}) — needs the app", report.display())),
    )
    .unwrap();
    assert!(v["cold_start_ms"].as_f64().unwrap() <= budget("startup", "cold_start_ms"));
}

/// R-PERF-004: open the reference note — first viewport ≤ 150 ms, full ≤ 500 ms.
#[test]
fn r_perf_004_open_reference_note() {
    let (db, note) = reference_db();
    let t = std::time::Instant::now();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    run(&mut e, "note.open", json!({"id": note.0}));
    let first = t.elapsed().as_secs_f64() * 1e3;
    assert!(
        first <= budget("open", "first_viewport_ms"),
        "first viewport {first:.0} ms"
    );
    assert!(first <= budget("open", "full_note_ms"));
}

/// R-PERF-004: the whole reference note is in memory (streamed after the first viewport)
/// within the full-note budget.
#[test]
fn r_perf_004_full_reference_note_loads() {
    let (db, note) = reference_db();
    let t = std::time::Instant::now();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    run(&mut e, "note.open", json!({"id": note.0}));
    e.wait_loaded();
    let full = t.elapsed().as_secs_f64() * 1e3;
    let want = budgets()["reference"]["note_strokes"].as_integer().unwrap() as usize;
    assert_eq!(e.session.ws.canvas(note).unwrap().objects.len(), want);
    assert!(full <= budget("open", "full_note_ms"), "full note {full:.0} ms");
}

/// R-PERF-005: search over the reference workspace.
#[test]
fn r_perf_005_search() {
    let (db, _) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let t = std::time::Instant::now();
    run(&mut e, "search.query", json!({"text": "Vorlesung"}));
    assert!(t.elapsed().as_secs_f64() * 1e3 <= budget("search", "complete_ms"));
}

/// R-PERF-007: memory with the reference note open ≤ budget.
#[test]
fn r_perf_007_memory() {
    let (db, _) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    e.load_all().unwrap();
    let mb = rss_mb();
    drop(e);
    assert!(mb <= budget("memory", "reference_note_open_mb"), "RSS {mb:.0} MB");
}

/// R-PERF-009: saving never blocks inking — handling a pen-up (which triggers a save) stays
/// within the inking budget.
#[test]
fn r_perf_009_save_never_blocks_inking() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = vibenote_engine::Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "p"})));
    run(&mut e, "note.open", json!({"id": n}));
    let f = fixture("fast-10.jsonl");
    let mut worst = 0.0f64;
    for ev in &f.events {
        let t = std::time::Instant::now();
        run(&mut e, "input.pen", json!({"events": [ev]}));
        worst = worst.max(t.elapsed().as_secs_f64() * 1e3);
    }
    assert!(
        worst <= budget("durability", "inking_while_saving_p99_ms"),
        "worst pen event {worst:.2} ms"
    );
}

/// R-PERF-010: a finished block is searchable within budget.
#[test]
fn r_perf_010_block_searchable() {
    let (mut e, _) = engine_with_note();
    replay(&mut e, &fixture("loops-4.jsonl"));
    let t = std::time::Instant::now();
    loop {
        let r = run(&mut e, "search.query", json!({"text": "e"}));
        if !r["results"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            t.elapsed().as_secs_f64() * 1e3 <= budget("hwr", "block_searchable_ms"),
            "not searchable in time"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// R-PERF-006 lives in pdf.rs (r_pdf_003), R-PERF-008 in org.rs (r_org_003).
#[test]
fn r_perf_006_pdf_page() {
    let (mut e, _) = engine_with_note();
    let t = std::time::Instant::now();
    let r = run(
        &mut e,
        "pdf.import",
        json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 0.0]}),
    );
    run(
        &mut e,
        "pdf.render_page",
        json!({"id": r["id"], "page": 1, "width_px": 1240}),
    );
    assert!(t.elapsed().as_secs_f64() * 1e3 <= budget("pdf", "page_visible_ms"));
}

#[test]
fn r_perf_008_quick_open() {
    let (db, _) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let t = std::time::Instant::now();
    run(&mut e, "quickopen.query", json!({"text": "Notiz 4711", "limit": 20}));
    assert!(t.elapsed().as_secs_f64() * 1e3 <= budget("quickopen", "per_keystroke_ms"));
}

/// R-PERF-001: input-to-ink latency [sim], simtab emit → presented frame (report written by
/// `cargo xtask bench latency`, which needs the app). Same budget as R-PEN-001.
#[test]
fn r_perf_001_latency() {
    let report = root().join("target/e2e-latency.json");
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&report)
            .unwrap_or_else(|_| panic!("no e2e latency report at {} — needs the app", report.display())),
    )
    .unwrap();
    assert!(v["p50_ms"].as_f64().unwrap() <= budget("latency", "p50_ms"));
    assert!(v["p95_ms"].as_f64().unwrap() <= budget("latency", "p95_ms"));
}
