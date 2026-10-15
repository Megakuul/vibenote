//! GPU-resident canvas state (M3.1, design `plan/design-m3-rendering.md` §1.4, §2).
//!
//! Strokes are encoded once ([`crate::encode`]) into per-cell **chunks**. A chunk holds its
//! strokes in (z, id) order in one or more **batches**; a batch owns three storage buffers
//! (points, runs, stroke table) and is drawn with one instanced draw. Work per frame is bounded
//! by a [`FrameBudget`] of counts (deterministic, no clocks): objects walked, samples encoded,
//! bytes uploaded. Content that is not ingested or uploaded yet is simply not drawn yet.
//!
//! The canvas arrives whole every frame (`Headless::render(&Canvas, …)`), so changes are found
//! without walking it: small canvases are hashed completely; large ones are identified by an
//! O(log n) key (address, length, first and last object) and re-checked by a budgeted verify
//! pass when the key changes. A large canvas mutated in place without changing its key needs
//! [`crate::Renderer::invalidate`] (the app's change feed, M3.1b, removes this caveat).

use crate::RenderError;
use crate::encode::{self, Cell, Encoded};
use std::collections::{BTreeSet, HashMap, VecDeque};
use vibenote_geom::{Point as P, Rect};
use vibenote_model::{Canvas, CanvasObject, ChangeLog, ObjectId, ObjectKind, Since};

/// Work allowed per frame (counts, so results depend only on inputs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameBudget {
    /// Objects the bulk/verify cursor visits.
    pub walk_objects: u32,
    /// Samples encoded (bulk ingest and chunk rebuilds).
    pub ingest_samples: u32,
    /// Bytes written to GPU buffers.
    pub upload_bytes: u64,
    /// Tiles rendered into the tile cache (ADR 0003: 8–16).
    pub tile_fills: u32,
}

impl FrameBudget {
    /// Sized for the debug test profile (design §2.4).
    pub const HEADLESS: FrameBudget = FrameBudget {
        walk_objects: 4_096,
        // ≈ 0.5 ms of encoding in the debug test profile: leaves margin for readback and GPU
        // waits in the 5.5 ms frame even on a busy machine (R-PERF-002).
        ingest_samples: 16_384,
        upload_bytes: 4 << 20,
        tile_fills: 2,
    };
    /// The app (release build): more ingest per frame.
    pub const APP: FrameBudget = FrameBudget {
        walk_objects: 16_384,
        ingest_samples: 262_144,
        upload_bytes: 8 << 20,
        tile_fills: 2,
    };
    /// The app while the pen is near or on the surface: no bulk loading, only edits (the
    /// user's own strokes), so ingest never delays ink (design risk 7).
    pub const PEN: FrameBudget = FrameBudget {
        walk_objects: 0,
        ingest_samples: 16_384,
        upload_bytes: 1 << 20,
        tile_fills: 2,
    };
    pub const UNLIMITED: FrameBudget = FrameBudget {
        walk_objects: u32::MAX,
        ingest_samples: u32::MAX,
        upload_bytes: u64::MAX,
        tile_fills: u32::MAX,
    };
}

/// What a frame did (tests and benches read it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Everything is ingested and uploaded: the frame shows the whole canvas.
    pub complete: bool,
    pub chunks: u32,
    pub chunks_visible: u32,
    /// Instances drawn (segments, including degenerate ones).
    pub instances: u64,
    pub ingested_samples: u64,
    pub upload_bytes: u64,
    /// GPU memory held by chunks.
    pub gpu_bytes: u64,
    /// Drawn from the tile cache (zoomed out, M3.3).
    pub tiled: bool,
    pub tiles_drawn: u32,
    pub tiles_filled: u32,
    /// Visible tiles shown through a fallback (coarser tile) or not at all this frame.
    pub tiles_missing: u32,
    /// The committed ink came from the cached frame (only live ink was drawn).
    pub cached: bool,
}

/// Objects at most this large are hashed completely each frame (exact change detection).
const SMALL_OBJECTS: usize = 4_096;
const SMALL_SAMPLES: usize = 65_536;
/// Per-batch limits (u16 run index; buffers ≤ 8 MiB of points).
const BATCH_POINTS: usize = 1 << 20;
const BATCH_RUNS: usize = 65_535;
/// GPU memory for chunks before invisible ones are evicted (R-PERF-007).
pub(crate) const MAX_GPU_BYTES: u64 = 512 << 20;
/// Chunks with more dead than this share of points are rebuilt.
const DEAD_SHARE: u32 = 4; // 1/4

pub(crate) const STROKE_DEAD: u32 = 1;
const STROKE_HIGHLIGHTER: u32 = 2;

fn fnv(h: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}
const FNV0: u64 = 0xcbf2_9ce4_8422_2325;

/// Every sample field that affects drawing (tilt widens tilt-responsive tools, R-PEN-003).
fn sample_hash(h: u64, p: &vibenote_model::Sample) -> u64 {
    let mut h = fnv(h, &p.x.to_bits().to_le_bytes());
    h = fnv(h, &p.y.to_bits().to_le_bytes());
    h = fnv(h, &p.pressure.to_bits().to_le_bytes());
    h = fnv(h, &p.tilt_x.to_bits().to_le_bytes());
    fnv(h, &p.tilt_y.to_bits().to_le_bytes())
}

/// O(1) fingerprint of an object (identity of its content for change detection).
fn fingerprint(o: &CanvasObject) -> u64 {
    let mut h = fnv(FNV0, &o.id.0.to_le_bytes());
    h = fnv(h, &o.z.to_le_bytes());
    match &o.kind {
        ObjectKind::Stroke(s) => {
            h = fnv(h, &[1, matches!(s.tool, vibenote_model::Tool::Highlighter).into()]);
            h = fnv(h, &s.color.0.to_le_bytes());
            h = fnv(h, &s.width.to_bits().to_le_bytes());
            h = fnv(h, &s.samples.len().to_le_bytes());
            h = fnv(h, &(s.samples.as_ptr() as usize).to_le_bytes());
            let n = s.samples.len();
            for k in [0, n / 2, n.saturating_sub(1)] {
                if let Some(p) = s.samples.get(k) {
                    h = sample_hash(h, p);
                }
            }
        }
        _ => h = fnv(h, &[2]),
    }
    h
}

/// Hash of everything that affects rendering, or `None` if the canvas is too large for it.
fn full_hash(c: &Canvas) -> Option<u64> {
    if c.objects.len() > SMALL_OBJECTS {
        return None;
    }
    let mut h = fnv(FNV0, &c.objects.len().to_le_bytes());
    let mut samples = 0usize;
    for o in c.objects.values() {
        h = fnv(h, &o.id.0.to_le_bytes());
        h = fnv(h, &o.z.to_le_bytes());
        if let ObjectKind::Stroke(s) = &o.kind {
            samples = samples.saturating_add(s.samples.len());
            if samples > SMALL_SAMPLES {
                return None;
            }
            h = fnv(h, &[matches!(s.tool, vibenote_model::Tool::Highlighter).into()]);
            h = fnv(h, &s.color.0.to_le_bytes());
            h = fnv(h, &s.width.to_bits().to_le_bytes());
            h = fnv(h, &s.samples.len().to_le_bytes());
            for p in &s.samples {
                h = sample_hash(h, p);
            }
        } else {
            h = fnv(h, &[9]);
        }
    }
    Some(h)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LargeKey {
    addr: usize,
    len: usize,
    first: u64,
    last: u64,
}

fn large_key(c: &Canvas) -> LargeKey {
    LargeKey {
        addr: std::ptr::from_ref(c) as usize,
        len: c.objects.len(),
        first: c.objects.values().next().map_or(0, fingerprint),
        last: c.objects.values().next_back().map_or(0, fingerprint),
    }
}

/// Where one share of an object lives.
#[derive(Clone, Copy, Debug)]
struct Slot {
    cell: Cell,
    hl: bool,
    batch: usize,
    stroke: u32,
    points: u32,
}

struct Record {
    fp: u64,
    z: i64,
    pass: u64,
    slots: Vec<Slot>,
    /// Every chunk the object belongs to (also evicted ones, which have no slots).
    cells: Vec<Cell>,
}

/// GPU side of a batch.
pub(crate) struct GpuBatch {
    pub points: wgpu::Buffer,
    pub runs: wgpu::Buffer,
    pub strokes: wgpu::Buffer,
    /// Kept point indices per LOD level (`vibenote_ink::lod::LOD_LEVELS`).
    pub lod: [wgpu::Buffer; 4],
    pub bind: wgpu::BindGroup,
}

/// New batches get fresh generations; their shares go to the LOD worker (M3.2).
#[derive(Default)]
pub(crate) struct LodCtx {
    next_gen: u64,
    pub jobs: Vec<crate::lod_worker::Job>,
}

pub(crate) struct Batch {
    /// Stroke table (color, r_max bits, flags, 0), mirrored on the CPU (small).
    strokes: Vec<[u32; 4]>,
    strokes_gpu: usize,
    strokes_dirty: bool,
    /// Run table (x bits, y bits, stroke slot, 0), mirrored.
    runs: Vec<[u32; 4]>,
    runs_gpu: usize,
    /// Points not uploaded yet.
    staged: Vec<[u32; 2]>,
    /// Points on the GPU (drawable prefix) and in total.
    pub points_gpu: u32,
    points_total: u32,
    dead_points: u32,
    pub gpu: Option<GpuBatch>,
    /// Identifies this batch's LOD results (stale ones are dropped).
    generation: u64,
    /// LOD lists received but not uploaded, per level.
    lod: [Vec<u32>; 4],
    /// Entries per level on the GPU.
    pub lod_gpu: [u32; 4],
    /// Points covered by received / uploaded LOD results (a prefix of the batch).
    lod_covered: u32,
    pub lod_gpu_covered: u32,
}

impl Batch {
    fn new(generation: u64) -> Batch {
        Batch {
            generation,
            lod: Default::default(),
            lod_gpu: [0; 4],
            lod_covered: 0,
            lod_gpu_covered: 0,
            strokes: Vec::new(),
            strokes_gpu: 0,
            strokes_dirty: false,
            runs: Vec::new(),
            runs_gpu: 0,
            staged: Vec::new(),
            points_gpu: 0,
            points_total: 0,
            dead_points: 0,
            gpu: None,
        }
    }
    fn fits(&self, sh: &encode::Share) -> bool {
        usize::try_from(self.points_total)
            .unwrap_or(usize::MAX)
            .saturating_add(sh.points.len())
            <= BATCH_POINTS
            && self.runs.len().saturating_add(sh.runs.len()) <= BATCH_RUNS
    }
    fn staged_bytes(&self) -> u64 {
        // Received LOD lists, or an empty result whose coverage still has to be published.
        let lod: usize = self
            .lod
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            .saturating_add(usize::from(self.lod_covered != self.lod_gpu_covered));
        let n = self
            .staged
            .len()
            .saturating_mul(8)
            .saturating_add(lod.saturating_mul(4))
            .saturating_add(self.runs.len().saturating_sub(self.runs_gpu).saturating_mul(16))
            .saturating_add(self.strokes.len().saturating_sub(self.strokes_gpu).saturating_mul(16));
        u64::try_from(n).unwrap_or(u64::MAX)
    }
    fn gpu_bytes(&self) -> u64 {
        self.gpu.as_ref().map_or(0, |g| {
            g.lod.iter().map(wgpu::Buffer::size).fold(
                g.points
                    .size()
                    .saturating_add(g.runs.size())
                    .saturating_add(g.strokes.size()),
                u64::saturating_add,
            )
        })
    }

    /// All points have their LOD lists on the GPU.
    fn lod_done(&self) -> bool {
        self.lod_gpu_covered >= self.points_total
    }
}

pub(crate) struct Chunk {
    pub cell: Cell,
    members: BTreeSet<(i64, ObjectId)>,
    pub pen: Vec<Batch>,
    pub hl: Vec<Batch>,
    pub bounds: Rect,
    order_ok: bool,
    last_visible: u64,
    evicted: bool,
    /// Changes whenever what the chunk draws changes (uploads, rebuilds): cached tiles made
    /// from it are stale when it differs (M3.3).
    pub version: u64,
}

impl Chunk {
    fn new(cell: Cell) -> Chunk {
        Chunk {
            cell,
            members: BTreeSet::new(),
            pen: Vec::new(),
            hl: Vec::new(),
            bounds: Rect::EMPTY,
            order_ok: true,
            last_visible: 0,
            evicted: false,
            version: 0,
        }
    }
    fn dead_heavy(&self) -> bool {
        let (dead, total) = self.pen.iter().chain(&self.hl).fold((0u32, 0u32), |(d, t), b| {
            (d.saturating_add(b.dead_points), t.saturating_add(b.points_total))
        });
        total > 0 && dead > total / DEAD_SHARE
    }
    pub fn gpu_bytes(&self) -> u64 {
        self.pen.iter().chain(&self.hl).map(Batch::gpu_bytes).sum()
    }
}

/// How the state follows the canvas.
enum Sync {
    None,
    /// Untracked, small: full hash.
    Small(u64),
    /// Untracked, large: O(log n) key.
    Large(LargeKey),
    /// Tracked by a change journal (the app): epoch and the revision last applied.
    Tracked {
        epoch: u64,
        rev: u64,
    },
}

pub(crate) struct State {
    sync: Sync,
    records: HashMap<ObjectId, Record>,
    pub chunks: HashMap<Cell, Chunk>,
    /// Bulk/verify pass: next id to visit (exclusive lower bound), pass number.
    cursor: Option<Option<ObjectId>>,
    pass: u64,
    /// Another pass is needed after the current one (the canvas changed during it).
    again: bool,
    rebuild: VecDeque<Cell>,
    pub lod: LodCtx,
    /// Changed objects to re-encode (tracked mode), deduplicated.
    edits: VecDeque<ObjectId>,
    /// Display transforms of strokes (prettify, ADR 0008): affine `[a, b, c, d, e, f]` mapping
    /// (x, y) to (a·x + c·y + e, b·x + d·y + f). The model's samples never change.
    pub display: HashMap<ObjectId, [f64; 6]>,
    edit_set: std::collections::HashSet<ObjectId>,
    pub frame: u64,
    /// Something drawn changed since the renderer last took this flag (cached frames are
    /// stale).
    pub dirty: bool,
}

/// Display transform changes applied as edits; more start over (cheaper in bulk).
const MAX_DISPLAY_EDITS: usize = 50_000;

impl State {
    pub fn new() -> State {
        State {
            display: HashMap::new(),
            sync: Sync::None,
            records: HashMap::new(),
            chunks: HashMap::new(),
            cursor: None,
            pass: 0,
            again: false,
            rebuild: VecDeque::new(),
            lod: LodCtx::default(),
            edits: VecDeque::new(),
            edit_set: std::collections::HashSet::new(),
            frame: 0,
            dirty: true,
        }
    }

    pub fn clear(&mut self) {
        let next_gen = self.lod.next_gen;
        let display = std::mem::take(&mut self.display);
        *self = State {
            frame: self.frame,
            pass: self.pass,
            display,
            ..State::new()
        };
        // Generations keep counting: results of dropped batches must never match new ones.
        self.lod.next_gen = next_gen;
    }

    /// Replaces the display transforms. Following a change journal, only strokes whose
    /// transform changed are re-encoded (a selection drag changes a few every frame); returns
    /// false if the state must start over instead.
    pub fn set_display(&mut self, display: HashMap<ObjectId, [f64; 6]>) -> bool {
        if !matches!(self.sync, Sync::Tracked { .. }) {
            return false;
        }
        let changed: Vec<ObjectId> = display
            .iter()
            .filter(|(id, t)| self.display.get(id) != Some(t))
            .map(|(id, _)| *id)
            .chain(self.display.keys().filter(|id| !display.contains_key(id)).copied())
            .collect();
        if changed.len() > MAX_DISPLAY_EDITS {
            return false;
        }
        self.display = display;
        for id in changed {
            if self.edit_set.insert(id) {
                self.edits.push_back(id);
            }
        }
        self.dirty = true;
        true
    }

    pub fn complete(&self) -> bool {
        self.cursor.is_none()
            && self.rebuild.is_empty()
            && self.edits.is_empty()
            && !self.again
            && self.chunks.values().all(|c| {
                c.pen
                    .iter()
                    .chain(&c.hl)
                    .all(|b| b.staged_bytes() == 0 && !b.strokes_dirty && b.lod_done())
            })
    }

    /// Brings the state up to date with `canvas` within `budget`; `changes` is the canvas's
    /// change journal when its owner keeps one. Returns samples encoded.
    pub fn sync(
        &mut self,
        canvas: &Canvas,
        changes: Option<&ChangeLog>,
        budget: &FrameBudget,
    ) -> Result<u64, RenderError> {
        self.frame = self.frame.saturating_add(1);
        if let Some(log) = changes {
            self.follow(log);
            return self.work(canvas, budget);
        }
        if let Some(h) = full_hash(canvas) {
            if !matches!(self.sync, Sync::Small(k) if k == h) {
                self.clear();
                self.sync = Sync::Small(h);
                let mut all: Vec<&CanvasObject> = canvas.objects.values().collect();
                all.sort_by(|a, b| a.z.cmp(&b.z).then(a.id.cmp(&b.id)));
                self.pass = self.pass.saturating_add(1);
                let mut n = 0u64;
                for o in all {
                    n = n.saturating_add(self.insert(o)?);
                }
                return Ok(n);
            }
            return Ok(0);
        }
        let key = large_key(canvas);
        match self.sync {
            Sync::Large(k) if k == key => {}
            Sync::Large(k) if k.addr == key.addr && self.cursor.is_none() => {
                self.sync = Sync::Large(key);
                self.changed(canvas)?;
            }
            Sync::Large(k) if k.addr == key.addr => {
                self.sync = Sync::Large(key);
                self.again = true;
            }
            _ => {
                // Another canvas: start over.
                self.clear();
                self.sync = Sync::Large(key);
                self.start_pass();
            }
        }
        self.work(canvas, budget)
    }

    /// Tracked mode: applies the journal since the last frame (O(changes)).
    fn follow(&mut self, log: &ChangeLog) {
        match self.sync {
            Sync::Tracked { epoch, rev } if epoch == log.epoch() => match log.since(rev) {
                Since::Current => {}
                Since::Changed(ids) => {
                    for id in ids {
                        if self.edit_set.insert(id) {
                            self.edits.push_back(id);
                        }
                    }
                }
                Since::TooOld => {
                    if self.cursor.is_some() {
                        self.again = true;
                    } else {
                        self.start_pass();
                    }
                }
            },
            _ => {
                self.clear();
                self.start_pass();
            }
        }
        self.sync = Sync::Tracked {
            epoch: log.epoch(),
            rev: log.rev(),
        };
    }

    /// Edits, rebuilds, then the bulk/verify pass, within the budget.
    fn work(&mut self, canvas: &Canvas, budget: &FrameBudget) -> Result<u64, RenderError> {
        let mut walked = 0u32;
        let mut samples = 0u64;
        let limit = u64::from(budget.ingest_samples);
        // Edits first: what the user just did shows up before anything else.
        if !self.edits.is_empty() {
            let mut todo: Vec<&CanvasObject> = Vec::new();
            while samples < limit {
                let Some(id) = self.edits.pop_front() else { break };
                self.edit_set.remove(&id);
                self.remove(id);
                if let Some(o) = canvas.objects.get(&id) {
                    if let ObjectKind::Stroke(s) = &o.kind {
                        samples = samples.saturating_add(u64::try_from(s.samples.len()).unwrap_or(u64::MAX));
                    }
                    todo.push(o);
                }
            }
            todo.sort_by(|a, b| a.z.cmp(&b.z).then(a.id.cmp(&b.id)));
            for o in todo {
                self.insert(o)?;
            }
            self.queue_fixes();
        }
        // Rebuilds (order fixes, compaction, evicted chunks coming back).
        while samples < limit {
            let Some(cell) = self.rebuild.pop_front() else { break };
            samples = samples.saturating_add(self.rebuild_chunk(cell, canvas)?);
        }
        if let Some(after) = self.cursor {
            let mut todo: Vec<&CanvasObject> = Vec::new();
            let mut last = after;
            let mut todo_samples = 0u64;
            let range = match after {
                Some(a) => canvas
                    .objects
                    .range((std::ops::Bound::Excluded(a), std::ops::Bound::Unbounded)),
                None => canvas.objects.range(..),
            };
            let mut finished = true;
            for (id, o) in range {
                if walked >= budget.walk_objects || samples.saturating_add(todo_samples) >= limit {
                    finished = false;
                    break;
                }
                walked = walked.saturating_add(1);
                last = Some(*id);
                let fp = fingerprint(o);
                let pass = self.pass;
                match self.records.get_mut(id) {
                    Some(r) if r.fp == fp => r.pass = pass,
                    _ => {
                        self.remove(*id);
                        if let ObjectKind::Stroke(s) = &o.kind {
                            todo_samples =
                                todo_samples.saturating_add(u64::try_from(s.samples.len()).unwrap_or(u64::MAX));
                        }
                        todo.push(o);
                    }
                }
            }
            todo.sort_by(|a, b| a.z.cmp(&b.z).then(a.id.cmp(&b.id)));
            for o in todo {
                samples = samples.saturating_add(self.insert(o)?);
            }
            self.cursor = if finished { None } else { Some(last) };
            if finished {
                self.end_pass();
            }
        }
        Ok(samples)
    }

    /// The key of a large canvas changed: new objects at the end are ingested at once (pen
    /// commits stay snappy); anything else starts a verify pass.
    fn changed(&mut self, canvas: &Canvas) -> Result<(), RenderError> {
        let max = self.records.keys().max().copied();
        let tail: Vec<&CanvasObject> = match max {
            Some(m) => canvas
                .objects
                .range((std::ops::Bound::Excluded(m), std::ops::Bound::Unbounded))
                .take(SMALL_OBJECTS.saturating_add(1))
                .map(|(_, o)| o)
                .collect(),
            None => Vec::new(),
        };
        if max.is_some() && tail.len() <= SMALL_OBJECTS {
            let mut tail = tail;
            tail.sort_by(|a, b| a.z.cmp(&b.z).then(a.id.cmp(&b.id)));
            for o in tail {
                self.insert(o)?;
            }
            if self.records.len() == canvas.objects.len() {
                self.queue_fixes();
                return Ok(());
            }
        }
        self.start_pass();
        Ok(())
    }

    /// Outside a pass, chunks whose order broke or that are mostly dead are rebuilt first.
    fn queue_fixes(&mut self) {
        if self.cursor.is_some() {
            return; // the pass end queues them
        }
        let mut fix: Vec<Cell> = self
            .chunks
            .values()
            .filter(|c| !c.order_ok || c.dead_heavy())
            .map(|c| c.cell)
            .collect();
        fix.sort_unstable();
        for cell in fix.into_iter().rev() {
            if !self.rebuild.contains(&cell) {
                self.rebuild.push_front(cell);
            }
        }
    }

    fn start_pass(&mut self) {
        self.pass = self.pass.saturating_add(1);
        self.cursor = Some(None);
        self.again = false;
    }

    fn end_pass(&mut self) {
        let pass = self.pass;
        let gone: Vec<ObjectId> = self
            .records
            .iter()
            .filter(|(_, r)| r.pass != pass)
            .map(|(k, _)| *k)
            .collect();
        for id in gone {
            self.remove(id);
        }
        for (cell, c) in &self.chunks {
            if (!c.order_ok || c.dead_heavy()) && !self.rebuild.contains(cell) {
                self.rebuild.push_back(*cell);
            }
        }
        if self.again {
            self.start_pass();
        }
    }

    /// Encodes and appends an object. Returns samples encoded.
    fn insert(&mut self, o: &CanvasObject) -> Result<u64, RenderError> {
        self.dirty = true;
        let ObjectKind::Stroke(s) = &o.kind else {
            self.records.insert(
                o.id,
                Record {
                    fp: fingerprint(o),
                    z: o.z,
                    pass: self.pass,
                    slots: Vec::new(),
                    cells: Vec::new(),
                },
            );
            return Ok(0);
        };
        // A stroke beyond the hostile-input bounds is not drawn (never a crash).
        let s = &*shown(&self.display, o.id, s);
        let enc = encode::encode(s).unwrap_or_else(|_| Encoded {
            color: 0,
            r_max: 0.0,
            highlighter: false,
            shares: Vec::new(),
        });
        let mut slots = Vec::with_capacity(enc.shares.len());
        let key = (o.z, o.id);
        for sh in &enc.shares {
            let chunk = self.chunks.entry(sh.cell).or_insert_with(|| Chunk::new(sh.cell));
            if chunk.members.last().is_some_and(|last| *last > key) {
                chunk.order_ok = false;
            }
            chunk.members.insert(key);
            chunk.bounds = union(chunk.bounds, sh.bounds);
            if chunk.evicted {
                continue; // drawn again when the chunk is rebuilt
            }
            slots.push(append(chunk, &enc, sh, &mut self.lod)?);
        }
        self.records.insert(
            o.id,
            Record {
                fp: fingerprint(o),
                z: o.z,
                pass: self.pass,
                slots,
                cells: enc
                    .shares
                    .iter()
                    .map(|s| s.cell)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            },
        );
        Ok(u64::try_from(s.samples.len()).unwrap_or(u64::MAX))
    }

    fn remove(&mut self, id: ObjectId) {
        let Some(r) = self.records.remove(&id) else { return };
        self.dirty = true;
        for sl in &r.slots {
            let Some(c) = self.chunks.get_mut(&sl.cell) else {
                continue;
            };
            let list = if sl.hl { &mut c.hl } else { &mut c.pen };
            if let Some(b) = list.get_mut(sl.batch)
                && let Some(e) = b.strokes.get_mut(usize::try_from(sl.stroke).unwrap_or(usize::MAX))
            {
                if let Some(f) = e.get_mut(2) {
                    *f |= STROKE_DEAD;
                }
                b.strokes_dirty = true;
                b.dead_points = b.dead_points.saturating_add(sl.points);
            }
        }
        for cell in &r.cells {
            if let Some(c) = self.chunks.get_mut(cell) {
                c.members.remove(&(r.z, id));
            }
        }
    }

    /// Re-encodes a chunk from the canvas in (z, id) order (order fix, compaction, return
    /// from eviction). Returns samples encoded.
    fn rebuild_chunk(&mut self, cell: Cell, canvas: &Canvas) -> Result<u64, RenderError> {
        self.dirty = true;
        let Some(chunk) = self.chunks.get_mut(&cell) else {
            return Ok(0);
        };
        let members: Vec<(i64, ObjectId)> = chunk.members.iter().copied().collect();
        chunk.version = chunk.version.wrapping_add(1);
        chunk.pen.clear();
        chunk.hl.clear();
        chunk.bounds = Rect::EMPTY;
        chunk.order_ok = true;
        chunk.evicted = false;
        chunk.members.clear();
        let mut n = 0u64;
        for (_, id) in members {
            // The old batches are gone: forget every member's slots here first, also for
            // members no longer in the canvas (a stale slot would later mark whatever stroke
            // took its place as dead).
            if let Some(rec) = self.records.get_mut(&id) {
                rec.slots.retain(|s| s.cell != cell);
            }
            let Some(o) = canvas.objects.get(&id) else { continue };
            if !self.records.contains_key(&id) {
                continue;
            }
            let ObjectKind::Stroke(s) = &o.kind else { continue };
            let s = &*shown(&self.display, id, s);
            let Ok(enc) = encode::encode(s) else { continue };
            n = n.saturating_add(u64::try_from(s.samples.len()).unwrap_or(u64::MAX));
            let Some(chunk) = self.chunks.get_mut(&cell) else { break };
            chunk.members.insert((o.z, id));
            for sh in enc.shares.iter().filter(|sh| sh.cell == cell) {
                chunk.bounds = union(chunk.bounds, sh.bounds);
                let slot = append(chunk, &enc, sh, &mut self.lod)?;
                if let Some(rec) = self.records.get_mut(&id) {
                    rec.slots.push(slot);
                }
            }
        }
        Ok(n)
    }

    /// Takes a LOD result: appended if its batch still exists and it is the next in order.
    pub fn apply_lod(&mut self, d: crate::lod_worker::Done) {
        let (cell, hl, bi, generation) = d.key;
        let Some(c) = self.chunks.get_mut(&cell) else { return };
        let list = if hl { &mut c.hl } else { &mut c.pen };
        let Some(b) = list.get_mut(bi) else { return };
        if b.generation != generation || b.lod_covered != d.base {
            return;
        }
        for (dst, src) in b.lod.iter_mut().zip(d.lists) {
            dst.extend(src);
        }
        b.lod_covered = b.lod_covered.saturating_add(d.count);
        self.dirty = true;
    }

    /// Drops the GPU data of the least recently visible invisible chunks until under `cap`.
    pub fn evict(&mut self, cap: u64) {
        let mut total: u64 = self.chunks.values().map(Chunk::gpu_bytes).sum();
        if total <= cap {
            return;
        }
        let frame = self.frame;
        let mut order: Vec<(u64, Cell)> = self
            .chunks
            .values()
            .filter(|c| c.last_visible != frame && !c.evicted)
            .map(|c| (c.last_visible, c.cell))
            .collect();
        order.sort_unstable();
        for (_, cell) in order {
            if total <= cap {
                break;
            }
            let Some(c) = self.chunks.get_mut(&cell) else { continue };
            total = total.saturating_sub(c.gpu_bytes());
            c.pen.clear();
            c.hl.clear();
            c.evicted = true;
            let ids: Vec<ObjectId> = c.members.iter().map(|(_, id)| *id).collect();
            for id in ids {
                if let Some(r) = self.records.get_mut(&id) {
                    r.slots.retain(|s| s.cell != cell);
                }
            }
        }
    }

    /// Marks chunks visible in `view` (canvas rect); evicted visible chunks are queued for a
    /// rebuild. Returns the visible cells.
    pub fn visible(&mut self, view: &Rect) -> Vec<Cell> {
        let frame = self.frame;
        let mut v = Vec::new();
        for c in self.chunks.values_mut() {
            if !c.bounds.is_empty()
                && c.bounds.min.x <= view.max.x
                && c.bounds.max.x >= view.min.x
                && c.bounds.min.y <= view.max.y
                && c.bounds.max.y >= view.min.y
            {
                c.last_visible = frame;
                v.push(c.cell);
                if c.evicted && !self.rebuild.contains(&c.cell) {
                    self.rebuild.push_front(c.cell);
                }
            }
        }
        v.sort_unstable();
        v
    }

    /// Uploads staged data within `budget` bytes. Growth copies old contents on the GPU.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        layout: &wgpu::BindGroupLayout,
        budget: u64,
        visible: &[Cell],
    ) -> u64 {
        let mut used = 0u64;
        // What is on screen first, then the rest in cell order.
        let mut cells: Vec<Cell> = visible.to_vec();
        let mut rest: Vec<Cell> = self.chunks.keys().filter(|c| !visible.contains(c)).copied().collect();
        rest.sort_unstable();
        cells.extend(rest);
        for cell in cells {
            let Some(c) = self.chunks.get_mut(&cell) else { continue };
            for b in c.pen.iter_mut().chain(c.hl.iter_mut()) {
                if used >= budget {
                    return used;
                }
                let n = upload_batch(b, device, queue, enc, layout, budget.saturating_sub(used));
                if n > 0 {
                    c.version = c.version.wrapping_add(1);
                }
                used = used.saturating_add(n);
            }
        }
        used
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    Rect {
        min: P::new(a.min.x.min(b.min.x), a.min.y.min(b.min.y)),
        max: P::new(a.max.x.max(b.max.x), a.max.y.max(b.max.y)),
    }
}

/// Appends one share to the chunk's last batch of its kind (or a new one).
fn append(chunk: &mut Chunk, enc: &Encoded, sh: &encode::Share, lod: &mut LodCtx) -> Result<Slot, RenderError> {
    let list = if enc.highlighter { &mut chunk.hl } else { &mut chunk.pen };
    if !list.last().is_some_and(|b| b.fits(sh)) {
        lod.next_gen = lod.next_gen.saturating_add(1);
        list.push(Batch::new(lod.next_gen));
    }
    let batch = list.len().saturating_sub(1);
    let b = list.last_mut().ok_or_else(|| RenderError::Viewport("batch".into()))?;
    let slot = u32::try_from(b.strokes.len()).map_err(|_| RenderError::Viewport("strokes".into()))?;
    let flags = if enc.highlighter { STROKE_HIGHLIGHTER } else { 0 };
    b.strokes.push([enc.color, enc.r_max.to_bits(), flags, 0]);
    let run_base = u32::try_from(b.runs.len()).map_err(|_| RenderError::Viewport("runs".into()))?;
    for r in &sh.runs {
        b.runs.push([r.x.to_bits(), r.y.to_bits(), slot, 0]);
    }
    for p in &sh.points {
        let run = run_base.saturating_add(p.run);
        let w0 = u32::from(p.dx.cast_unsigned()) | (u32::from(p.dy.cast_unsigned()) << 16);
        let w1 = (run & 0xffff) | (u32::from(p.r) << 16) | (u32::from(p.flags) << 24);
        b.staged.push([w0, w1]);
    }
    let n = u32::try_from(sh.points.len()).map_err(|_| RenderError::Viewport("points".into()))?;
    // Decoded points for the LOD worker (chunk-local units; radius from the 8-bit value).
    let base = b.points_total;
    let mut pts = Vec::with_capacity(sh.points.len());
    for p in &sh.points {
        let run = sh.runs.get(usize::try_from(p.run).unwrap_or(usize::MAX));
        let (rx, ry) = run.map_or((0.0, 0.0), |r| (f64::from(r.x), f64::from(r.y)));
        pts.push([
            rx + f64::from(p.dx) / 1024.0,
            ry + f64::from(p.dy) / 1024.0,
            f64::from(enc.r_max) * f64::from(p.r) / 255.0,
        ]);
    }
    lod.jobs.push(crate::lod_worker::Job {
        key: (chunk.cell, enc.highlighter, batch, b.generation),
        base,
        points: pts,
        flags: sh.points.iter().map(|p| p.flags).collect(),
    });
    b.points_total = b.points_total.saturating_add(n);
    // A share always starts a piece (START or CONT), so instances never read across shares.
    // `encode` guarantees it; it is not re-checked here.
    Ok(Slot {
        cell: chunk.cell,
        hl: enc.highlighter,
        batch,
        stroke: slot,
        points: n,
    })
}

fn bytes_of<T: bytemuck::Pod>(v: &[T]) -> &[u8] {
    bytemuck::cast_slice(v)
}

/// Capacity for `need` bytes: doubling, at least 4 KiB, 256-byte aligned.
fn capacity(cur: u64, need: u64) -> u64 {
    let mut c = cur.max(4096);
    while c < need {
        c = c.saturating_mul(2);
    }
    c.saturating_add(255) & !255
}

fn new_buffer(device: &wgpu::Device, size: u64, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// Uploads one batch's staged data (tables first, then as many points as fit). Returns bytes.
fn upload_batch(
    b: &mut Batch,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    enc: &mut wgpu::CommandEncoder,
    layout: &wgpu::BindGroupLayout,
    budget: u64,
) -> u64 {
    if b.staged_bytes() == 0 && !b.strokes_dirty {
        return 0;
    }
    let as_u64 = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let lod_need = |b: &Batch, l: usize| {
        let n = b.lod.get(l).map_or(0, Vec::len);
        let have = b.lod_gpu.get(l).copied().unwrap_or(0);
        u64::from(have).saturating_add(as_u64(n)).saturating_mul(4)
    };
    let need = [
        as_u64(usize::try_from(b.points_total).unwrap_or(usize::MAX).saturating_mul(8)),
        as_u64(b.runs.len().saturating_mul(16)),
        as_u64(b.strokes.len().saturating_mul(16)),
        lod_need(b, 0),
        lod_need(b, 1),
        lod_need(b, 2),
        lod_need(b, 3),
    ];
    let cur = b.gpu.as_ref().map(|g| {
        [
            g.points.size(),
            g.runs.size(),
            g.strokes.size(),
            g.lod[0].size(),
            g.lod[1].size(),
            g.lod[2].size(),
            g.lod[3].size(),
        ]
    });
    let grow = cur.is_none_or(|c| c.iter().zip(&need).any(|(have, want)| have < want));
    if grow {
        let c = cur.unwrap_or([0; 7]);
        let size = |k: usize| capacity(c.get(k).copied().unwrap_or(0), need.get(k).copied().unwrap_or(0));
        let points = new_buffer(device, size(0), "ink points");
        let runs = new_buffer(device, size(1), "ink runs");
        let strokes = new_buffer(device, size(2), "ink strokes");
        let lod = [3usize, 4, 5, 6].map(|k| new_buffer(device, size(k), "ink lod"));
        if let Some(old) = &b.gpu {
            // Points and LOD lists are not mirrored on the CPU: copy the uploaded prefixes on the
            // GPU. Tables are mirrored and simply rewritten below.
            let n = u64::from(b.points_gpu).saturating_mul(8);
            if n > 0 {
                enc.copy_buffer_to_buffer(&old.points, 0, &points, 0, n);
            }
            for ((o, n), have) in old.lod.iter().zip(&lod).zip(b.lod_gpu) {
                let bytes = u64::from(have).saturating_mul(4);
                if bytes > 0 {
                    enc.copy_buffer_to_buffer(o, 0, n, 0, bytes);
                }
            }
        }
        fn entry(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buf.as_entire_binding(),
            }
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ink batch"),
            layout,
            entries: &[
                entry(0, &points),
                entry(1, &runs),
                entry(2, &strokes),
                entry(3, &lod[0]),
                entry(4, &lod[1]),
                entry(5, &lod[2]),
                entry(6, &lod[3]),
            ],
        });
        b.gpu = Some(GpuBatch {
            points,
            runs,
            strokes,
            lod,
            bind,
        });
        b.runs_gpu = 0;
        b.strokes_gpu = 0;
        b.strokes_dirty = false;
    }
    let Some(g) = &b.gpu else { return 0 };
    let mut used = 0u64;
    // Tables first (whole tables when dirty: they are small).
    if b.strokes_dirty {
        queue.write_buffer(&g.strokes, 0, bytes_of(&b.strokes));
        used = used.saturating_add(as_u64(b.strokes.len().saturating_mul(16)));
        b.strokes_gpu = b.strokes.len();
        b.strokes_dirty = false;
    } else if b.strokes_gpu < b.strokes.len() {
        let from = b.strokes_gpu;
        queue.write_buffer(
            &g.strokes,
            as_u64(from.saturating_mul(16)),
            bytes_of(b.strokes.get(from..).unwrap_or(&[])),
        );
        used = used.saturating_add(as_u64(b.strokes.len().saturating_sub(from).saturating_mul(16)));
        b.strokes_gpu = b.strokes.len();
    }
    if b.runs_gpu < b.runs.len() {
        let from = b.runs_gpu;
        queue.write_buffer(
            &g.runs,
            as_u64(from.saturating_mul(16)),
            bytes_of(b.runs.get(from..).unwrap_or(&[])),
        );
        used = used.saturating_add(as_u64(b.runs.len().saturating_sub(from).saturating_mul(16)));
        b.runs_gpu = b.runs.len();
    }
    // LOD lists (whole results, small next to the points).
    for ((buf, list), have) in g.lod.iter().zip(b.lod.iter_mut()).zip(b.lod_gpu.iter_mut()) {
        if list.is_empty() {
            continue;
        }
        queue.write_buffer(buf, u64::from(*have).saturating_mul(4), bytes_of(list));
        used = used.saturating_add(as_u64(list.len().saturating_mul(4)));
        *have = have.saturating_add(u32::try_from(list.len()).unwrap_or(u32::MAX));
        list.clear();
    }
    b.lod_gpu_covered = b.lod_covered;
    // Points: as many as the budget allows (at least one page, so huge batches progress).
    let room = usize::try_from(budget.saturating_sub(used) / 8)
        .unwrap_or(usize::MAX)
        .max(4096);
    let n = b.staged.len().min(room);
    if n > 0 {
        let at = u64::from(b.points_gpu).saturating_mul(8);
        queue.write_buffer(&g.points, at, bytes_of(b.staged.get(..n).unwrap_or(&[])));
        b.staged.drain(..n);
        b.points_gpu = b.points_gpu.saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
        used = used.saturating_add(as_u64(n.saturating_mul(8)));
    }
    used
}

/// A stroke as displayed: its display transform applied, if it has one.
fn shown<'a>(
    display: &HashMap<ObjectId, [f64; 6]>,
    id: ObjectId,
    s: &'a vibenote_model::Stroke,
) -> std::borrow::Cow<'a, vibenote_model::Stroke> {
    match display.get(&id) {
        Some(&[a, b, c, d, e, f]) => {
            let mut t = s.clone();
            for p in &mut t.samples {
                let (x, y) = (p.x, p.y);
                p.x = a * x + c * y + e;
                p.y = b * x + d * y + f;
            }
            std::borrow::Cow::Owned(t)
        }
        None => std::borrow::Cow::Borrowed(s),
    }
}
