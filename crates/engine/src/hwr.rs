//! Handwriting in the engine (ADR 0006–0008): the recogniser and classifier loaded once
//! (lazily, off the pen path), and a background job thread with cancellation (R-HWR-008).
//!
//! Assets come from `$VIBENOTE_ASSETS` or, in development, the repository's `assets/`
//! directory (packaging puts them next to the binary later).

use crate::EngineError;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use vibenote_hwr::ctc::Decoded;
use vibenote_hwr::raster::Gray;
use vibenote_hwr::recognizer::Recognizer;
use vibenote_hwr::trees::Model;

/// The assets directory.
pub fn assets_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("VIBENOTE_ASSETS") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(3) {
            let p = dir.join("assets");
            if p.join("packs").is_dir() {
                return p;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

/// The bundled packs' directory.
/// Most written blocks kept in memory, and blocks queued by one sweep.
const MAX_BLOCKS: usize = 100_000;
const MAX_SWEEP: usize = 1_000;
/// Most strokes of cleared blocks remembered for an undo.
pub(crate) const MAX_CLEARED: usize = 100_000;

pub fn packs_dir() -> PathBuf {
    assets_dir().join("packs")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Recognition of one block: its lines' text, joined by newlines.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct BlockText {
    pub text: String,
    pub lines: Vec<String>,
    /// Mean character confidence (0–1).
    pub conf: f32,
    /// Pack version tag.
    pub tag: String,
}

/// A recognition job's state.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done { result: BlockText },
    Failed { error: String },
    Cancelled,
}

struct Job {
    id: u64,
    note: vibenote_model::NoteId,
    /// The block's first stroke (its identity in the search index).
    block: vibenote_model::ObjectId,
    region: vibenote_geom::Rect,
    /// The block's stroke points; split into lines and rasterised by the worker (never on the
    /// thread that submits, which is the input thread at pen-up).
    strokes: Vec<Vec<(f64, f64)>>,
    cancel: Arc<AtomicBool>,
}

/// A writing block that prettify would tidy (mode "suggest"), waiting for the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Suggestion {
    pub note: vibenote_model::NoteId,
    pub block: u64,
}

/// Open suggestions kept per session (oldest dropped first).
const MAX_SUGGESTIONS: usize = 256;

/// A finished background recognition, for the search index.
#[derive(Clone, Debug)]
pub struct Finished {
    pub note: vibenote_model::NoteId,
    pub block: vibenote_model::ObjectId,
    pub region: vibenote_geom::Rect,
    pub result: BlockText,
}

struct Shared {
    recognizer: OnceLock<Result<Recognizer, String>>,
    classifier: OnceLock<Result<Model, String>>,
    queue: Mutex<VecDeque<Job>>,
    states: Mutex<BTreeMap<u64, (JobState, Arc<AtomicBool>)>>,
    finished: Mutex<Vec<Finished>>,
    /// Version tag of the pack that recognises (`None`: no pack with a recogniser).
    tag: OnceLock<Option<String>>,
    wake: Condvar,
    stop: AtomicBool,
}

struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
        self.0.wake.notify_all();
    }
}

/// The handwriting service of a session (cheap to clone).
#[derive(Clone)]
pub struct HwrService {
    shared: Arc<Shared>,
    started: Arc<Mutex<bool>>,
    next_job: Arc<Mutex<u64>>,
    _guard: Arc<StopOnDrop>,
}

impl Default for HwrService {
    fn default() -> Self {
        let shared = Arc::new(Shared {
            recognizer: OnceLock::new(),
            classifier: OnceLock::new(),
            queue: Mutex::default(),
            states: Mutex::default(),
            finished: Mutex::default(),
            tag: OnceLock::new(),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        HwrService {
            _guard: Arc::new(StopOnDrop(Arc::clone(&shared))),
            shared,
            started: Arc::default(),
            next_job: Arc::new(Mutex::new(1)),
        }
    }
}

impl std::fmt::Debug for HwrService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HwrService")
    }
}

impl PartialEq for HwrService {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// The version tag of the pack the recogniser uses (reads manifests only).
fn current_tag() -> Option<String> {
    let (packs, _) = vibenote_hwr::pack::discover(&[packs_dir()]);
    packs
        .iter()
        .find(|p| p.manifest.model.is_some())
        .map(vibenote_hwr::pack::Pack::tag)
}

fn load_recognizer() -> Result<Recognizer, String> {
    let (packs, errors) = vibenote_hwr::pack::discover(&[packs_dir()]);
    let pack = packs.iter().find(|p| p.manifest.model.is_some()).ok_or_else(|| {
        format!(
            "no language pack with a recogniser in {} ({})",
            packs_dir().display(),
            errors.join("; ")
        )
    })?;
    Recognizer::load(pack)
}

fn recognize_lines(r: &Recognizer, lines: &[Gray], cancel: Option<&AtomicBool>) -> Result<Option<BlockText>, String> {
    let mut out = Vec::new();
    let mut conf = Vec::new();
    for l in lines {
        if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
            return Ok(None);
        }
        let d: Decoded = r.recognize(l)?;
        conf.extend(d.conf.iter().copied());
        out.push(d.text);
    }
    #[allow(clippy::cast_precision_loss)] // character counts
    let mean = if conf.is_empty() {
        0.0
    } else {
        conf.iter().sum::<f32>() / conf.len() as f32
    };
    Ok(Some(BlockText {
        text: out.join("\n"),
        lines: out,
        conf: mean,
        tag: r.tag.clone(),
    }))
}

impl HwrService {
    /// The recogniser (loaded on first use).
    pub fn recognizer(&self) -> Result<&Recognizer, EngineError> {
        self.shared
            .recognizer
            .get_or_init(load_recognizer)
            .as_ref()
            .map_err(|e| EngineError::Model(e.clone()))
    }

    /// The writing-vs-drawing classifier (gate A).
    pub fn classifier(&self) -> Result<&Model, EngineError> {
        self.shared
            .classifier
            .get_or_init(|| {
                let p = assets_dir().join("hwr/classifier.json");
                let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                Model::parse(&s)
            })
            .as_ref()
            .map_err(|e| EngineError::Model(e.clone()))
    }

    /// Recognises line images now.
    pub fn recognize_now(&self, lines: &[Gray]) -> Result<BlockText, EngineError> {
        let r = self.recognizer()?;
        recognize_lines(r, lines, None)
            .map_err(EngineError::Model)?
            .ok_or_else(|| EngineError::Model("cancelled".into()))
    }

    /// Queues a block for background recognition; returns the job id.
    /// Version tag stored with what this pack reads (blocks read by another are read again).
    pub fn current_tag(&self) -> Option<String> {
        self.shared.tag.get_or_init(current_tag).clone()
    }

    pub fn submit(
        &self,
        note: vibenote_model::NoteId,
        block: vibenote_model::ObjectId,
        region: vibenote_geom::Rect,
        strokes: Vec<Vec<(f64, f64)>>,
    ) -> u64 {
        let id = {
            let mut n = lock(&self.next_job);
            let id = *n;
            *n = n.saturating_add(1);
            id
        };
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.shared.states).insert(id, (JobState::Queued, Arc::clone(&cancel)));
        lock(&self.shared.queue).push_back(Job {
            id,
            note,
            block,
            region,
            strokes,
            cancel,
        });
        self.start();
        self.shared.wake.notify_one();
        id
    }

    /// Cancels a job (queued or running); false if it is unknown or already finished.
    pub fn cancel(&self, id: u64) -> bool {
        let mut states = lock(&self.shared.states);
        match states.get_mut(&id) {
            Some((st @ (JobState::Queued | JobState::Running), c)) => {
                c.store(true, Ordering::Release);
                *st = JobState::Cancelled;
                true
            }
            _ => false,
        }
    }

    pub fn state(&self, id: u64) -> Option<JobState> {
        lock(&self.shared.states).get(&id).map(|(s, _)| s.clone())
    }

    /// Recognitions finished in the background since the last call.
    pub fn take_finished(&self) -> Vec<Finished> {
        std::mem::take(&mut *lock(&self.shared.finished))
    }

    /// True while jobs wait or run.
    pub fn busy(&self) -> bool {
        !lock(&self.shared.queue).is_empty()
            || lock(&self.shared.states)
                .values()
                .any(|(s, _)| matches!(s, JobState::Running))
    }

    fn start(&self) {
        let mut started = lock(&self.started);
        if *started {
            return;
        }
        *started = true;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new().name("vibenote-hwr".into()).spawn(move || {
            loop {
                let job = {
                    let mut q = lock(&shared.queue);
                    loop {
                        if shared.stop.load(Ordering::Acquire) {
                            return;
                        }
                        if let Some(j) = q.pop_front() {
                            break j;
                        }
                        q = shared.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                if job.cancel.load(Ordering::Acquire) {
                    continue;
                }
                if let Some((st, _)) = lock(&shared.states).get_mut(&job.id) {
                    *st = JobState::Running;
                }
                let r = shared.recognizer.get_or_init(load_recognizer);
                let outcome = match r {
                    Ok(r) => {
                        let lines = crate::commands::hwr::block_lines(&job.strokes);
                        recognize_lines(r, &lines, Some(&job.cancel))
                    }
                    Err(e) => Err(e.clone()),
                };
                let state = match outcome {
                    Ok(Some(result)) => {
                        lock(&shared.finished).push(Finished {
                            note: job.note,
                            block: job.block,
                            region: job.region,
                            result: result.clone(),
                        });
                        JobState::Done { result }
                    }
                    Ok(None) => JobState::Cancelled,
                    Err(error) => JobState::Failed { error },
                };
                let mut states = lock(&shared.states);
                if let Some((st, _)) = states.get_mut(&job.id)
                    && !matches!(st, JobState::Cancelled)
                {
                    *st = state;
                }
                // Keep the job table bounded.
                while states.len() > 10_000 {
                    let Some(k) = states.keys().next().copied() else { break };
                    states.remove(&k);
                }
            }
        });
        if spawned.is_err() {
            *started = false;
        }
    }
}

/// A greyscale image file as a 48 px-high line (eval sets: photographed or scanned lines).
pub fn line_from_image(bytes: &[u8]) -> Result<Gray, EngineError> {
    let img = image::load_from_memory(bytes).map_err(|e| EngineError::Model(format!("image: {e}")))?;
    let g = img.to_luma32f();
    let (w, h) = g.dimensions();
    if w == 0 || h == 0 || w > 20_000 || h > 2_000 {
        return Err(EngineError::Model("line image size out of range".into()));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // ≤ 3200 px wide
    let nw = ((f64::from(w) * 48.0 / f64::from(h)).ceil() as u32).clamp(1, 3200);
    let r = image::imageops::resize(&g, nw, 48, image::imageops::FilterType::Triangle);
    Ok(Gray {
        width: usize::try_from(nw).unwrap_or(1),
        height: 48,
        px: r.into_raw(),
    })
}

/// The block being written (strokes in time order), closed after the idle threshold, a jump
/// elsewhere or a note change, then recognised in the background.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct OpenBlock {
    pub note: vibenote_model::NoteId,
    pub strokes: Vec<vibenote_model::ObjectId>,
    pub last_end_us: u64,
    /// Union of the strokes' extents and the median stroke height.
    #[serde(skip)]
    pub bbox: vibenote_geom::Rect,
    #[serde(skip)]
    pub heights: Vec<f64>,
}

/// A jump this many times the typical stroke height (at least 120 units) starts a new block.
const JUMP_FACTOR: f64 = 6.0;
const MIN_JUMP: f64 = 120.0;

impl crate::Session {
    /// A committed pen stroke joins the open block or starts a new one.
    pub(crate) fn block_add(
        &mut self,
        note: vibenote_model::NoteId,
        id: vibenote_model::ObjectId,
        start_us: u64,
        end_us: u64,
    ) {
        let Some(b) = self
            .ws
            .canvas(note)
            .ok()
            .and_then(|c| c.objects.get(&id))
            .and_then(|o| o.kind.bounds())
        else {
            return;
        };
        let idle = u64::from(self.settings.block_idle_ms).saturating_mul(1000);
        let continues = self.open_block.as_ref().is_some_and(|o| {
            let mut hs = o.heights.clone();
            hs.sort_by(f64::total_cmp);
            let typical = hs.get(hs.len() / 2).copied().unwrap_or(0.0);
            let dx = (o.bbox.min.x - b.max.x).max(b.min.x - o.bbox.max.x).max(0.0);
            let dy = (o.bbox.min.y - b.max.y).max(b.min.y - o.bbox.max.y).max(0.0);
            o.note == note
                && start_us.saturating_sub(o.last_end_us) <= idle
                && dx.hypot(dy) <= (JUMP_FACTOR * typical).max(MIN_JUMP)
        });
        if !continues {
            self.block_close();
        }
        let o = self.open_block.get_or_insert_with(|| OpenBlock {
            note,
            strokes: Vec::new(),
            last_end_us: end_us,
            bbox: vibenote_geom::Rect::EMPTY,
            heights: Vec::new(),
        });
        o.strokes.push(id);
        o.last_end_us = o.last_end_us.max(end_us);
        o.bbox.include(b.min);
        o.bbox.include(b.max);
        o.heights.push(b.max.y - b.min.y);
    }

    /// Closes the open block and queues it for recognition (no-op without one).
    pub(crate) fn block_close(&mut self) {
        let Some(o) = self.open_block.take() else { return };
        let Ok(c) = self.ws.canvas(o.note) else { return };
        let strokes: Vec<Vec<(f64, f64)>> = o
            .strokes
            .iter()
            .filter_map(|id| match c.objects.get(id).map(|x| &x.kind) {
                Some(vibenote_model::ObjectKind::Stroke(st)) => Some(st.samples.iter().map(|p| (p.x, p.y)).collect()),
                _ => None,
            })
            .collect();
        let (Some(first), false) = (o.strokes.first().copied(), strokes.is_empty()) else {
            return;
        };
        let region = crate::commands::hwr::region(&strokes);
        self.block_members
            .insert((o.note, first), o.strokes.iter().map(|i| i.0).collect());
        if self.persistent() {
            self.derived_pending.push(vibenote_storage::Derived::InkBlock {
                note: o.note,
                block: first,
                members: o.strokes.clone(),
            });
        }
        self.bound_blocks();
        self.hwr.submit(o.note, first, region, strokes);
    }

    /// Prettify (ADR 0008, R-HWR-006): a recognised block that passes both gates is
    /// prettified in mode "automatic" (one undo step), and offered as a suggestion in mode
    /// "suggest" (the UI shows a chip that applies it). Nothing in mode "off".
    fn auto_prettify(&mut self, f: &Finished) {
        use vibenote_model::settings::PrettifyMode;
        let mode = self.settings.prettify_mode;
        if mode == PrettifyMode::Off || !crate::commands::hwr::gate_b(&f.result) {
            return;
        }
        let Some(ids) = self.block_strokes(f.note, f.block) else {
            return;
        };
        let Ok(pts) = crate::commands::hwr::stroke_points(self, f.note, &ids) else {
            return;
        };
        let gate_a = self
            .hwr
            .classifier()
            .ok()
            .zip(vibenote_hwr::features::features(&pts))
            .is_some_and(|(m, x)| m.predict(&x) >= m.threshold);
        if !gate_a {
            return;
        }
        if mode == PrettifyMode::Automatic {
            let _ = crate::commands::hwr::prettify(self, f.note, &ids);
            return;
        }
        let s = Suggestion {
            note: f.note,
            block: f.block.0,
        };
        if !self.suggestions.contains(&s) {
            if self.suggestions.len() >= MAX_SUGGESTIONS {
                self.suggestions.remove(0);
            }
            self.suggestions.push(s);
        }
    }

    /// Open prettify suggestions of `note` with the box of their strokes as shown now; blocks
    /// already prettified, or whose strokes are gone, are left out.
    pub fn prettify_suggestions(&self, note: vibenote_model::NoteId) -> Vec<(u64, vibenote_geom::Rect)> {
        let Ok(c) = self.ws.canvas(note) else { return Vec::new() };
        let done = self.ws.prettified.get(&note);
        let display = self.prettify_transforms(note);
        self.suggestions
            .iter()
            .filter(|s| s.note == note)
            .filter(|s| !done.is_some_and(|d| d.contains_key(&vibenote_model::ObjectId(s.block))))
            .filter_map(|s| {
                let ids = self.block_strokes(note, vibenote_model::ObjectId(s.block))?;
                let mut r = vibenote_geom::Rect::EMPTY;
                for id in &ids {
                    let id = vibenote_model::ObjectId(*id);
                    let Some(vibenote_model::ObjectKind::Stroke(st)) = c.objects.get(&id).map(|o| &o.kind) else {
                        return None; // changed since: no longer a suggestion
                    };
                    let b = st.bounds();
                    let b = display.get(&id).map_or(b, |t| map_rect(t, &b));
                    r = r.union(&b);
                }
                (!r.is_empty()).then_some((s.block, r))
            })
            .collect()
    }

    /// Takes suggestion `block` of `note` out of the list; returns its strokes if it was open.
    pub(crate) fn take_suggestion(&mut self, note: vibenote_model::NoteId, block: u64) -> Option<Vec<u64>> {
        let i = self
            .suggestions
            .iter()
            .position(|s| s.note == note && s.block == block)?;
        self.suggestions.remove(i);
        self.block_strokes(note, vibenote_model::ObjectId(block))
    }

    /// The strokes of a recognised block (kept with the job; first stroke = the block id).
    fn block_strokes(&self, note: vibenote_model::NoteId, block: vibenote_model::ObjectId) -> Option<Vec<u64>> {
        self.block_members.get(&(note, block)).cloned()
    }

    /// Display transforms of prettified strokes in a note (for the renderer).
    pub fn prettify_transforms(
        &self,
        note: vibenote_model::NoteId,
    ) -> std::collections::HashMap<vibenote_model::ObjectId, [f64; 6]> {
        let mut out = std::collections::HashMap::new();
        for p in self.ws.prettified.get(&note).into_iter().flat_map(|m| m.values()) {
            for (s, t) in p.strokes.iter().zip(&p.transforms) {
                out.insert(*s, *t);
            }
        }
        out
    }

    /// Prettified strokes of `note` whose displayed extent meets `area` (hit-testing works on
    /// what is shown; the spatial index holds the stored, untransformed boxes).
    pub(crate) fn prettified_near(
        &self,
        note: vibenote_model::NoteId,
        display: &std::collections::HashMap<vibenote_model::ObjectId, [f64; 6]>,
        area: &vibenote_geom::Rect,
    ) -> Vec<vibenote_model::ObjectId> {
        let Ok(c) = self.ws.canvas(note) else { return Vec::new() };
        let mut out: Vec<vibenote_model::ObjectId> = display
            .iter()
            .filter(|(id, t)| {
                c.objects
                    .get(id)
                    .and_then(|o| o.kind.bounds())
                    .is_some_and(|b| map_rect(t, &b).intersects(area))
            })
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Keeps the block maps bounded; a note whose blocks were dropped is read again on use.
    fn bound_blocks(&mut self) {
        while self.block_members.len() > MAX_BLOCKS {
            let Some(k) = self.block_members.keys().next().copied() else {
                break;
            };
            self.block_members.remove(&k);
            self.block_tags.remove(&k);
            self.blocks_loaded.remove(&k.0);
        }
    }

    /// Reads a note's written blocks and the pack version that read each (once per note).
    pub(crate) fn ensure_blocks(&mut self, note: vibenote_model::NoteId) {
        if !self.persistent() || self.blocks_loaded.contains(&note) {
            return;
        }
        let Some(Ok(blocks)) = self.reader.with(|st| st.ink_blocks(note)) else {
            return; // derived data: tried again on the next use
        };
        self.blocks_loaded.insert(note);
        for (block, members, tag) in blocks {
            self.block_members
                .entry((note, block))
                .or_insert_with(|| members.iter().map(|m| m.0).collect());
            if let Some(t) = tag {
                self.block_tags.entry((note, block)).or_insert(t);
            }
        }
        self.bound_blocks();
    }

    /// True if any of a block's strokes exists.
    fn block_alive(&self, note: vibenote_model::NoteId, block: vibenote_model::ObjectId) -> bool {
        let Ok(c) = self.ws.canvas(note) else { return false };
        match self.block_members.get(&(note, block)) {
            Some(m) => m
                .iter()
                .any(|id| c.objects.contains_key(&vibenote_model::ObjectId(*id))),
            None => c.objects.contains_key(&block),
        }
    }

    /// Strokes were removed: blocks with none of their strokes left leave the search index
    /// and count as not read (an undo brings the strokes back and the block is read again).
    pub(crate) fn ink_strokes_removed(&mut self, note: vibenote_model::NoteId, removed: &[vibenote_model::ObjectId]) {
        self.ensure_blocks(note);
        let lo = (note, vibenote_model::ObjectId(0));
        let hi = (note, vibenote_model::ObjectId(u64::MAX));
        let hit: Vec<vibenote_model::ObjectId> = self
            .block_members
            .range(lo..=hi)
            .filter(|(_, m)| m.iter().any(|id| removed.iter().any(|r| r.0 == *id)))
            .map(|((_, b), _)| *b)
            .collect();
        for block in hit {
            if self.block_alive(note, block) {
                continue;
            }
            self.block_tags.remove(&(note, block));
            if let Some(m) = self.block_members.get(&(note, block)) {
                for id in m.iter().take(crate::hwr::MAX_CLEARED) {
                    self.cleared_strokes.insert(vibenote_model::ObjectId(*id), note);
                }
            }
            if self.cleared_strokes.len() > MAX_CLEARED {
                self.cleared_strokes.clear(); // bounded; those blocks are re-read on the next open
            }
            if self.persistent() {
                self.derived_pending
                    .push(vibenote_storage::Derived::InkClear { note, block });
            } else if let Some(ix) = self.mem_index().as_mut() {
                let _ = ix.set_ink_text(note, block, vibenote_geom::Rect::EMPTY, "", "");
            }
        }
    }

    /// Queues recognition for a complete note's blocks that were never read, lost their text
    /// or were read by another pack version. Returns the number queued.
    pub(crate) fn sweep_ink(&mut self, note: vibenote_model::NoteId) -> usize {
        let Some(tag) = self.hwr.current_tag() else { return 0 };
        self.ensure_blocks(note);
        let lo = (note, vibenote_model::ObjectId(0));
        let hi = (note, vibenote_model::ObjectId(u64::MAX));
        let todo: Vec<(vibenote_model::ObjectId, Vec<u64>)> = self
            .block_members
            .range(lo..=hi)
            .filter(|(k, _)| self.block_tags.get(k) != Some(&tag))
            .map(|((_, b), m)| (*b, m.clone()))
            .take(MAX_SWEEP)
            .collect();
        let Ok(c) = self.ws.canvas(note) else { return 0 };
        let mut jobs = Vec::new();
        for (block, members) in todo {
            let strokes: Vec<Vec<(f64, f64)>> = members
                .iter()
                .filter_map(
                    |id| match c.objects.get(&vibenote_model::ObjectId(*id)).map(|o| &o.kind) {
                        Some(vibenote_model::ObjectKind::Stroke(st)) => {
                            Some(st.samples.iter().map(|p| (p.x, p.y)).collect())
                        }
                        _ => None,
                    },
                )
                .collect();
            if strokes.is_empty() {
                continue;
            }
            let region = crate::commands::hwr::region(&strokes);
            jobs.push((block, region, strokes));
        }
        let n = jobs.len();
        for (block, region, strokes) in jobs {
            self.hwr.submit(note, block, region, strokes);
        }
        n
    }

    /// Records background recognitions that finished (search index, storage).
    pub(crate) fn take_recognitions(&mut self) -> Result<usize, EngineError> {
        let done = self.hwr.take_finished();
        let n = done.len();
        for f in done {
            // A block none of whose strokes is left (erased meanwhile) is not indexed.
            if self.block_alive(f.note, f.block) {
                self.set_ink_text(f.note, f.block, f.region, &f.result)?;
                self.block_tags.insert((f.note, f.block), f.result.tag.clone());
                self.auto_prettify(&f);
            }
        }
        Ok(n)
    }
}

/// `t` applied to a point (affine `[a, b, c, d, e, f]`: x' = a·x + c·y + e, y' = b·x + d·y + f).
pub(crate) fn map_point(t: &[f64; 6], (x, y): (f64, f64)) -> (f64, f64) {
    (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5])
}

/// The box around a transformed box.
pub(crate) fn map_rect(t: &[f64; 6], r: &vibenote_geom::Rect) -> vibenote_geom::Rect {
    let mut out = vibenote_geom::Rect::EMPTY;
    for p in [
        (r.min.x, r.min.y),
        (r.max.x, r.min.y),
        (r.min.x, r.max.y),
        (r.max.x, r.max.y),
    ] {
        let (x, y) = map_point(t, p);
        out.include(vibenote_geom::Point::new(x, y));
    }
    out
}

/// The inverse of an affine transform (`None` if it is degenerate).
pub(crate) fn invert(t: &[f64; 6]) -> Option<[f64; 6]> {
    let det = t[0] * t[3] - t[1] * t[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (t[3] / det, -t[1] / det, -t[2] / det, t[0] / det);
    Some([a, b, c, d, -(a * t[4] + c * t[5]), -(b * t[4] + d * t[5])])
}
