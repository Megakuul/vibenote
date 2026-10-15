//! Storage thread (R-PERF-009, R-DATA-001): the engine hands applied ops to a channel and
//! returns at once; one thread owns the [`Store`] and commits whatever has queued up in a single
//! transaction (group commit: a batch waits at most for the commit before it, far below the
//! 250 ms bound). Every batch carries a sequence number; the thread publishes the highest
//! durable one so callers can wait for it ([`Persister::sync`]).
//!
//! Failed commits keep their ops (in order) and are retried with backoff; the error is visible
//! in [`Status`] until a retry succeeds. Ops are never dropped, except when the process exits
//! while the disk keeps failing (then [`Persister::sync`] has already reported the error).

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use vibenote_model::{Op, Settings};
use vibenote_storage::{HistoryOp, Store};

/// Commit attempts for the remaining ops when shutting down with a failing disk.
const SHUTDOWN_ATTEMPTS: u32 = 3;
const BACKOFF_MIN: Duration = Duration::from_millis(20);
const BACKOFF_MAX: Duration = Duration::from_secs(2);

/// Storage progress, readable at `/storage` in `state.inspect`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Status {
    /// Highest batch handed to the storage thread.
    pub submitted: u64,
    /// Highest batch committed durably (every batch ≤ this one is on disk).
    pub durable: u64,
    /// Last commit error, cleared by the next successful commit.
    pub error: Option<String>,
    /// Commits done (each may cover several batches).
    pub commits: u64,
}

#[derive(Default)]
struct Shared {
    durable: u64,
    /// Highest batch included in the last failed attempt, and its error.
    failed: Option<(u64, String)>,
    commits: u64,
    /// The thread has exited (no more progress).
    stopped: bool,
}

enum Msg {
    Batch {
        seq: u64,
        ops: Vec<Op>,
        hist: Vec<HistoryOp>,
        next_id: u64,
        settings: Option<Settings>,
        /// Named session values (key, JSON), see `vibenote_storage::META_KEYS`.
        meta: Vec<(&'static str, String)>,
        derived: Vec<vibenote_storage::Derived>,
    },
    Shutdown,
}

/// A batch the storage thread could not take (it has stopped), handed back so the caller keeps
/// it instead of dropping it.
#[derive(Default)]
pub struct Rejected {
    pub ops: Vec<Op>,
    pub hist: Vec<HistoryOp>,
    pub settings: Option<Settings>,
    pub meta: Vec<(&'static str, String)>,
    pub derived: Vec<vibenote_storage::Derived>,
}

pub struct Persister {
    tx: Sender<Msg>,
    submitted: u64,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    // A poisoned lock only means the storage thread panicked mid-update; the counters are
    // still meaningful (monotonic), so keep going rather than propagate the panic.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Persister {
    /// `recovery`: where to write uncommitted changes if the last commits at shutdown fail
    /// (first path that works; see `vibenote_storage::recovery`).
    pub fn spawn(store: Store, recovery: Vec<std::path::PathBuf>) -> Result<Persister, std::io::Error> {
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let sh = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("vibenote-storage".into())
            .spawn(move || {
                run(store, &rx, &sh, &recovery);
                lock(&sh.0).stopped = true;
                sh.1.notify_all();
            })?;
        Ok(Persister {
            tx,
            submitted: 0,
            shared,
            thread: Some(thread),
        })
    }

    /// Hands `ops` to the storage thread. Never blocks on storage (no lock, no I/O).
    pub fn submit(
        &mut self,
        ops: Vec<Op>,
        hist: Vec<HistoryOp>,
        next_id: u64,
        settings: Option<Settings>,
        meta: Vec<(&'static str, String)>,
        derived: Vec<vibenote_storage::Derived>,
    ) -> Result<u64, Box<Rejected>> {
        let seq = self.submitted.saturating_add(1);
        let sent = self.tx.send(Msg::Batch {
            seq,
            ops,
            hist,
            next_id,
            settings,
            meta,
            derived,
        });
        match sent {
            Ok(()) => {
                self.submitted = seq;
                Ok(seq)
            }
            Err(std::sync::mpsc::SendError(Msg::Batch {
                ops,
                hist,
                settings,
                meta,
                derived,
                ..
            })) => Err(Box::new(Rejected {
                ops,
                hist,
                settings,
                meta,
                derived,
            })),
            Err(std::sync::mpsc::SendError(Msg::Shutdown)) => Err(Box::default()),
        }
    }

    /// Waits until everything submitted so far is durable. Returns the commit error if an
    /// attempt covering it failed (the ops stay queued and are retried).
    pub fn sync(&self) -> Result<(), String> {
        let target = self.submitted;
        let (m, cv) = &*self.shared;
        let mut g = lock(m);
        loop {
            if g.durable >= target {
                return Ok(());
            }
            if let Some((upto, e)) = &g.failed
                && *upto >= target
            {
                return Err(e.clone());
            }
            if g.stopped {
                return Err("storage thread stopped".into());
            }
            g = cv.wait(g).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub fn status(&self) -> Status {
        let g = lock(&self.shared.0);
        Status {
            submitted: self.submitted,
            durable: g.durable,
            error: g.failed.as_ref().map(|(_, e)| e.clone()),
            commits: g.commits,
        }
    }
}

impl Drop for Persister {
    fn drop(&mut self) {
        // Finish queued commits so a reopened workspace sees everything.
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Ops waiting to be committed, oldest first.
#[derive(Default)]
struct Queue {
    ops: Vec<Op>,
    hist: Vec<HistoryOp>,
    upto: u64,
    next_id: u64,
    /// Newest settings not yet stored (later batches replace earlier ones).
    settings: Option<Settings>,
    /// Newest named session values not yet stored (conveniences: not kept for recovery).
    meta: std::collections::BTreeMap<&'static str, String>,
    /// Derived data not yet stored (recomputable: not kept for recovery).
    derived: Vec<vibenote_storage::Derived>,
}

impl Queue {
    fn push(&mut self, seq: u64, ops: Vec<Op>, hist: Vec<HistoryOp>, next_id: u64, meta: Meta) {
        let (settings, values, derived) = meta;
        self.derived.extend(derived);
        if settings.is_some() {
            self.settings = settings;
        }
        self.meta.extend(values);
        self.ops.extend(ops);
        self.hist.extend(hist);
        self.upto = self.upto.max(seq);
        self.next_id = self.next_id.max(next_id);
    }
}

/// Settings and named session values carried by a batch.
type Meta = (
    Option<Settings>,
    Vec<(&'static str, String)>,
    Vec<vibenote_storage::Derived>,
);

fn run(mut store: Store, rx: &Receiver<Msg>, shared: &(Mutex<Shared>, Condvar), recovery: &[std::path::PathBuf]) {
    let mut q = Queue::default();
    let mut backoff = BACKOFF_MIN;
    let mut shutdown = false;
    loop {
        // Wait for work: block when idle, otherwise only for the retry backoff.
        let first = if q.upto == 0 {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(backoff)
        };
        match first {
            Ok(Msg::Batch {
                seq,
                ops,
                hist,
                next_id,
                settings,
                meta,
                derived,
            }) => q.push(seq, ops, hist, next_id, (settings, meta, derived)),
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => shutdown = true,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // Group commit: take everything already queued.
        for m in rx.try_iter() {
            match m {
                Msg::Batch {
                    seq,
                    ops,
                    hist,
                    next_id,
                    settings,
                    meta,
                    derived,
                } => q.push(seq, ops, hist, next_id, (settings, meta, derived)),
                Msg::Shutdown => shutdown = true,
            }
        }
        let mut attempts = 0u32;
        while q.upto > 0 {
            let meta: Vec<(&str, &str)> = q.meta.iter().map(|(k, v)| (*k, v.as_str())).collect();
            match store.apply_batch_meta(&q.ops, &q.hist, q.next_id, q.settings.as_ref(), &meta, &q.derived) {
                Ok(()) => {
                    let mut g = lock(&shared.0);
                    g.durable = g.durable.max(q.upto);
                    g.failed = None;
                    g.commits = g.commits.saturating_add(1);
                    drop(g);
                    shared.1.notify_all();
                    q = Queue::default();
                    backoff = BACKOFF_MIN;
                }
                Err(e) => {
                    lock(&shared.0).failed = Some((q.upto, e.to_string()));
                    shared.1.notify_all();
                    backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);
                    attempts = attempts.saturating_add(1);
                    if shutdown && attempts < SHUTDOWN_ATTEMPTS {
                        std::thread::sleep(backoff);
                        continue;
                    }
                }
            }
            break;
        }
        if shutdown {
            if q.upto > 0 {
                save_for_recovery(q, recovery);
            }
            return;
        }
    }
}

/// The disk would not take the last changes: keep them in a recovery file (next to the
/// workspace, else the fallback location) for the next open to apply.
fn save_for_recovery(q: Queue, paths: &[std::path::PathBuf]) {
    let r = vibenote_storage::Recovery {
        ops: q.ops,
        hist: q.hist,
        next_id: q.next_id,
        settings: q.settings,
    };
    for p in paths {
        if vibenote_storage::write_recovery(p, &r).is_ok() {
            return;
        }
    }
}
