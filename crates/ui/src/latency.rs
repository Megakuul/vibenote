//! Opt-in latency telemetry (ADR 0001 rule 4): `VIBENOTE_LATENCY_LOG=<path>` writes JSON lines
//! with pen-event receive times, frame submits and `wp_presentation` results, all on
//! CLOCK_MONOTONIC. `cargo xtask bench latency` matches them with simtab's emit log
//! (method: docs/spikes/input-results.md §3). Disabled → no work beyond one `Option` check.

use std::io::Write;
use vibenote_ink::input::{PenEvent, PenEventKind};

pub struct LatencyLog {
    w: std::io::BufWriter<std::fs::File>,
    /// Index of the next pen event.
    next_in: u64,
    next_frame: u64,
}

fn kind(k: PenEventKind) -> &'static str {
    match k {
        PenEventKind::ProximityIn => "prox_in",
        PenEventKind::ProximityOut => "prox_out",
        PenEventKind::Down => "down",
        PenEventKind::Motion => "motion",
        PenEventKind::Up => "up",
        PenEventKind::ButtonDown => "btn_down",
        PenEventKind::ButtonUp => "btn_up",
    }
}

impl LatencyLog {
    /// From `VIBENOTE_LATENCY_LOG`, if set and creatable.
    pub fn from_env() -> Option<LatencyLog> {
        let p = std::env::var_os("VIBENOTE_LATENCY_LOG")?;
        match std::fs::File::create(&p) {
            Ok(f) => Some(LatencyLog {
                w: std::io::BufWriter::with_capacity(1 << 20, f),
                next_in: 0,
                next_frame: 0,
            }),
            Err(e) => {
                eprintln!("vibenote: latency log: {e}");
                None
            }
        }
    }

    fn line(&mut self, v: &serde_json::Value) {
        // Telemetry must never disturb the app: write errors are ignored.
        let _ = serde_json::to_writer(&mut self.w, v);
        let _ = self.w.write_all(b"\n");
    }

    pub fn meta(&mut self, v: serde_json::Value) {
        self.line(&serde_json::json!({"k": "meta", "v": v}));
    }

    /// Logs a received pen event; returns its index.
    pub fn input(&mut self, e: &PenEvent, ns: u64) -> u64 {
        let i = self.next_in;
        self.next_in = i.saturating_add(1);
        self.line(&serde_json::json!({
            "k": "ev", "i": i, "ns": ns, "t": kind(e.kind),
            "x": e.x, "y": e.y, "p": e.p, "tx": e.tx, "ty": e.ty,
        }));
        i
    }

    /// Index of the newest pen event logged so far (-1: none).
    pub fn last_input(&self) -> i64 {
        i64::try_from(self.next_in).map_or(i64::MAX, |n| n.saturating_sub(1))
    }

    /// Logs a submitted frame (drawn up to the newest input); returns its id.
    pub fn draw(&mut self, ns: u64) -> u64 {
        let f = self.next_frame;
        self.next_frame = f.saturating_add(1);
        let last = self.last_input();
        self.line(&serde_json::json!({"k": "draw", "f": f, "ns": ns, "last": last}));
        f
    }

    pub fn presented(&mut self, f: u64, ns: u64, refresh_ns: u32) {
        self.line(&serde_json::json!({"k": "pres", "f": f, "ns": ns, "refresh": refresh_ns}));
    }

    pub fn discarded(&mut self, f: u64) {
        self.line(&serde_json::json!({"k": "disc", "f": f}));
    }

    pub fn flush(&mut self) {
        let _ = self.w.flush();
    }
}
