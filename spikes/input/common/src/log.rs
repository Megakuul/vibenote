//! JSONL event/frame log written by a background thread (keeps file I/O off the input path).
//!
//! Records (all `ns` are CLOCK_MONOTONIC):
//! - `{"k":"meta", ...}`                           variant + settings
//! - `{"k":"ev","i",ns,"t",tool,x,y,p,tx,ty,d,ms,hist}` input event as received by the app
//!   (`x`,`y` surface-local logical px; `ms` = protocol timestamp if any; `hist` = recovered from
//!   toolkit motion history/coalescing)
//! - `{"k":"draw","f",ns,"last"}`                  frame `f` submitted containing events `..=last`
//! - `{"k":"pres","f",ns,"refresh","flags"}`       frame `f` presented at `ns` (wp_presentation)
//! - `{"k":"disc","f"}`                            frame `f` discarded

use std::io::Write;
use std::sync::mpsc;

pub struct Log {
    tx: Option<mpsc::Sender<String>>,
    th: Option<std::thread::JoinHandle<()>>,
    next_ev: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Ev {
    pub x: f64,
    pub y: f64,
    pub p: f64,
    pub tx: f64,
    pub ty: f64,
    pub d: f64,
}

impl Log {
    /// Opens `$INK_LOG` (if set). Without it, logging is a no-op.
    pub fn from_env() -> Self {
        Self::from_env_suffix("")
    }

    /// Like `from_env` but writes to `$INK_LOG<suffix>` (for a second writer in the same app).
    pub fn from_env_suffix(suffix: &str) -> Self {
        let Ok(path) = std::env::var("INK_LOG").map(|p| p + suffix) else {
            return Log { tx: None, th: None, next_ev: 0 };
        };
        let (tx, rx) = mpsc::channel::<String>();
        let th = std::thread::spawn(move || {
            let Ok(f) = std::fs::File::create(&path) else { return };
            let mut w = std::io::BufWriter::new(f);
            // Flush whenever idle for 50 ms: spike apps are often killed with SIGTERM.
            loop {
                match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(l) => {
                        let _ = writeln!(w, "{l}");
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let _ = w.flush();
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = w.flush();
        });
        Log { tx: Some(tx), th: Some(th), next_ev: 0 }
    }

    pub fn raw(&self, line: String) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(line);
        }
    }

    pub fn meta(&self, v: serde_json::Value) {
        let mut v = v;
        v["k"] = "meta".into();
        self.raw(v.to_string());
    }

    /// Logs an input event; returns its index.
    #[allow(clippy::too_many_arguments)]
    pub fn ev(&mut self, ns: u64, t: &str, tool: &str, e: Ev, ms: Option<u32>, hist: bool) -> u64 {
        let i = self.next_ev;
        self.next_ev += 1;
        if self.tx.is_some() {
            let ms = ms.map(|m| m.to_string()).unwrap_or_else(|| "null".into());
            self.raw(format!(
                r#"{{"k":"ev","i":{i},"ns":{ns},"t":"{t}","tool":"{tool}","x":{:.3},"y":{:.3},"p":{:.5},"tx":{:.2},"ty":{:.2},"d":{:.4},"ms":{ms},"hist":{hist}}}"#,
                e.x, e.y, e.p, e.tx, e.ty, e.d
            ));
        }
        i
    }

    pub fn last_ev(&self) -> Option<u64> {
        self.next_ev.checked_sub(1)
    }

    pub fn draw(&self, f: u64, ns: u64, last: Option<u64>) {
        let last = last.map(|l| l.to_string()).unwrap_or_else(|| "null".into());
        self.raw(format!(r#"{{"k":"draw","f":{f},"ns":{ns},"last":{last}}}"#));
    }

    pub fn pres(&self, f: u64, ns: u64, refresh_ns: u64, flags: u32) {
        self.raw(format!(r#"{{"k":"pres","f":{f},"ns":{ns},"refresh":{refresh_ns},"flags":{flags}}}"#));
    }

    pub fn disc(&self, f: u64) {
        self.raw(format!(r#"{{"k":"disc","f":{f}}}"#));
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(th) = self.th.take() {
            let _ = th.join();
        }
    }
}
