#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Crash safety (R-DATA-001, AGENTS.md §2 Phase 3 item 7): a writer commits strokes and prints
//! `ACK <id>` after each durable commit; we SIGKILL it at pseudo-random moments, reopen the
//! workspace, and require integrity + every acknowledged stroke present.
//! Iterations: $VIBENOTE_CRASH_ITERS (default 25; `cargo xtask bench` runs 1000).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use vibenote_model::ObjectId;

#[test]
fn kill_9_never_loses_acknowledged_strokes() {
    let iters: u32 = std::env::var("VIBENOTE_CRASH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut acked: Vec<u64> = vec![];
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    for it in 0..iters {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let run_ms = 20 + seed % 300;
        let mut child = Command::new(env!("CARGO_BIN_EXE_vibenote-cli"))
            .args(["--workspace", db.to_str().unwrap(), "crash-writer"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(id) = line.strip_prefix("ACK ").and_then(|s| s.parse::<u64>().ok()) {
                    let _ = tx.send(id);
                }
            }
        });
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(run_ms) {
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap(); // SIGKILL
        child.wait().unwrap();
        reader.join().unwrap();
        acked.extend(rx.try_iter());
        let store = vibenote_storage::Store::open(&db).unwrap();
        assert_eq!(store.integrity_check().unwrap(), "ok", "iteration {it}");
        let ws = store.load().unwrap();
        let (note, canvas) = ws.canvases.iter().next().expect("note exists");
        // Ops and their undo steps commit in one transaction: one step per stroke (≤ 1,000).
        let steps = store.load_history(&format!("note:{}", note.0)).unwrap().undo.len();
        assert_eq!(
            steps,
            canvas.objects.len().min(1000),
            "iteration {it}: undo log out of step with the stored strokes"
        );
        for id in &acked {
            assert!(
                canvas.objects.contains_key(&ObjectId(*id)),
                "iteration {it}: acknowledged stroke {id} lost"
            );
        }
    }
    assert!(
        acked.len() > usize::try_from(iters).unwrap(),
        "writer made too little progress ({} acks)",
        acked.len()
    );
}
