//! LOD worker (M3.2, design §4.1): one thread owned by the renderer computes the level lists of
//! newly encoded shares, so Douglas–Peucker never runs on the render (pen) thread. Results carry
//! the batch generation; stale ones are dropped by the receiver.

use std::sync::mpsc::{Receiver, Sender};
use vibenote_ink::lod::{LOD_LEVELS, importance, kept};

/// Identifies a batch: cell, highlighter list, index in the chunk, generation.
pub(crate) type BatchKey = (crate::encode::Cell, bool, usize, u64);

pub(crate) struct Job {
    pub key: BatchKey,
    /// Batch-local index of the share's first point.
    pub base: u32,
    /// Decoded points (chunk-local x, y, radius) and their flags.
    pub points: Vec<[f64; 3]>,
    pub flags: Vec<u8>,
}

pub(crate) struct Done {
    pub key: BatchKey,
    pub base: u32,
    pub count: u32,
    /// Batch-local kept point indices per level (`LOD_LEVELS` order).
    pub lists: [Vec<u32>; 4],
}

pub(crate) struct LodWorker {
    jobs: Option<Sender<Job>>,
    done: Option<Receiver<Done>>,
    /// Set on drop: the thread stops after its current job instead of draining the queue.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Jobs sent and not yet received back.
    pub outstanding: u64,
}

impl LodWorker {
    pub fn spawn() -> Option<LodWorker> {
        let (jtx, jrx) = std::sync::mpsc::channel::<Job>();
        let (dtx, drx) = std::sync::mpsc::channel::<Done>();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let halt = std::sync::Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("vibenote-lod".into())
            .spawn(move || {
                for job in jrx {
                    if halt.load(std::sync::atomic::Ordering::Acquire) || dtx.send(run(job)).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(LodWorker {
            jobs: Some(jtx),
            done: Some(drx),
            stop,
            thread: Some(thread),
            outstanding: 0,
        })
    }

    pub fn send(&mut self, job: Job) {
        if let Some(tx) = &self.jobs
            && tx.send(job).is_ok()
        {
            self.outstanding = self.outstanding.saturating_add(1);
        }
    }

    /// Results ready now.
    pub fn try_done(&mut self) -> Option<Done> {
        let d = self.done.as_ref()?.try_recv().ok()?;
        self.outstanding = self.outstanding.saturating_sub(1);
        Some(d)
    }

    /// The next result, waiting for it (`None` if nothing is outstanding or the thread died).
    pub fn wait_done(&mut self) -> Option<Done> {
        if self.outstanding == 0 {
            return None;
        }
        let d = self.done.as_ref()?.recv().ok()?;
        self.outstanding = self.outstanding.saturating_sub(1);
        Some(d)
    }
}

impl Drop for LodWorker {
    fn drop(&mut self) {
        // Stop after the current job: the backlog is not wanted any more.
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.jobs = None;
        self.done = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Splits the share into pieces (each starts at a START or CONT point) and keeps, per level,
/// each piece's important points; piece starts and ends are always kept.
pub(crate) fn run(job: Job) -> Done {
    let mut lists: [Vec<u32>; 4] = Default::default();
    let mut imp = Vec::new();
    let mut idx = Vec::new();
    let n = job.points.len();
    let mut start = 0usize;
    while start < n {
        // A piece ends where a new one starts (START/CONT) and wherever its segments switch
        // between owned and seam copies (a point's SEAM bit describes the segment ending there).
        // Piece ends are kept at every level, so a dropped point never merges owned and seam
        // segments into one chord (below zoom ≈ 0.027 cells draw unscissored and skip seams).
        let seam = |i: usize| job.flags.get(i).map(|f| f & crate::encode::F_SEAM);
        let mut end = start.saturating_add(1);
        while end < n
            && job
                .flags
                .get(end)
                .is_some_and(|f| f & (crate::encode::F_START | crate::encode::F_CONT) == 0)
            && (end == start.saturating_add(1) || seam(end) == seam(end.saturating_sub(1)))
        {
            end = end.saturating_add(1);
        }
        let piece = job.points.get(start..end).unwrap_or(&[]);
        importance(piece, &mut imp);
        for (li, &level) in LOD_LEVELS.iter().enumerate() {
            idx.clear();
            kept(&imp, level, &mut idx);
            if let Some(list) = lists.get_mut(li) {
                let off = job.base.saturating_add(u32::try_from(start).unwrap_or(u32::MAX));
                list.extend(idx.iter().map(|i| off.saturating_add(*i)));
            }
        }
        start = end;
    }
    Done {
        key: job.key,
        base: job.base,
        count: u32::try_from(n).unwrap_or(u32::MAX),
        lists,
    }
}
