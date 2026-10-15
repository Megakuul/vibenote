# Design note — M3 "Rendering at scale" (M3.1–M3.3)

- **Role:** architect. **Date:** 2026-10-13. **Status:** draft for implementation. One part
  (§1.2, the point format) differs from a parameter in locked ADR 0003 decision 4. It needs an owner
  decision first (§9, D1).
- **Builds on:** ADR 0003 (Accepted, locked), ADR 0001 (locked), `plan/architecture.md` §2/§4,
  SPEC R-PERF-002, R-CANVAS-001/005, R-PEN-005.
- **Scope:** `crates/render` (M3.1–M3.3), `crates/ink` (M3.2 decimation). It also needs a small
  change feed in `model` + `engine` + `ui`. That is proposed as a new row, M3.1b (§6.3).
- All measurements are **[sim]** in the ADR 0003 sense: the real GPU (AMD Radeon 860M, RADV
  KRACKAN1, Mesa 26.2.4) with synthetic data. They were taken in the debug test profile
  (workspace crates opt-level 0, dependencies opt-level 2) unless marked otherwise.

---

## 0. Feasibility measurements (done first)

Temporary probes (`crates/render/examples/m3_probe.rs`, `apps/vibenote-cli/examples/m3_quant.rs`)
were run with `CARGO_TARGET_DIR=/dev/shm/vibenote-target` and deleted afterwards. The probe
rebuilt the R-PERF-000 reference note with the exact generator from
`tests/acceptance/src/lib.rs` (100,000 strokes, 7,963,388 samples).

### 0.1 Readback floor: `Headless::render` of an empty canvas, 1920×1200

| Run | Calls 1…10 (ms) |
|---|---|
| fresh `Headless`, 10 calls | **6.12**, 2.53, 1.25, 1.08, 1.04, 1.04, 0.99, 1.02, 1.01, 1.00 |
| 3 more runs, perf-test zoom sequence (5 calls) | **5.91**, 2.51, 1.38, 1.10, 1.07 · **6.59**, 2.66, 1.15, 1.07, 1.08 · **6.34**, 2.66, 1.22, 1.10, 1.07 |

Phase breakdown (replica of `Headless::render` with timers):

| Phase | 1st call | Steady state |
|---|---|---|
| create texture + readback buffer | 1.94 | 0.01 (0.00 if reused) |
| encode (first pipeline creation included) | 0.35 | 0.01 |
| submit + texture→buffer copy + map wait | 1.09 | 0.47 |
| row copy into the `Vec` (page faults on the first 9.2 MB allocation) | 1.54 | 0.48–0.55 |
| **total** | **~4.9–6.6** | **0.96–1.05** (reused target), 1.03–1.30 (fresh) |

Empty submit + wait: 0.018 ms. Upload (`queue.write_buffer` + submit + wait): 1 MB 0.10 ms,
4 MB 0.29 ms, 8 MB 0.64 ms, 32 MB 3.6 ms.

Warm-up variants (a dummy render inside `Headless::new`, then the perf-test sequence on an empty
canvas, 3 runs):

| Warm-up in `new()` | Call 1 (ms) | Calls 2–5 (ms) |
|---|---|---|
| none | 5.91 / 6.59 / 6.34 | 1.07–2.66 |
| 64×64 frame | 2.58 / 2.04 / 2.04 | 1.03–1.33 |
| 256×256 frame | 2.32 / 1.97 / 2.03 | 1.00–1.58 |
| 1920×1200 frame | 2.03 / 1.56 / 1.57 | 1.03–1.38 |

**Conclusion:** readback alone fits the 5.5 ms budget. Steady state is about 1.0 ms. Without
warm-up, the **first** call on an empty canvas already takes 5.9–6.6 ms, which is over budget.
Two changes are therefore mandatory:
1. `Headless::new` warms up. It creates all pipelines and renders and reads back one 256×256
   frame. This is untimed by the test and matches M1.4 "GPU pipelines cached" at startup.
2. `Headless` caches the target texture and the readback buffer per size.

With both, the first frame costs about 2.0–2.6 ms and later frames about 1.0–1.3 ms. That leaves
**~3 ms of the 5.5 ms for culling, ingest, uploads, draw and tile fills.** There is no reason to
stop and report a readback limit to the owner.

### 0.2 What the current 340 ms consists of (reference note, debug profile)

| | opt-level 0 (today) | render at opt-level 2 (`--config`) |
|---|---|---|
| `Headless::render`, zooms 1/1/0.5/0.1/0.037 | 335 / 329 / 336 / 343 / 360 ms | 196 / 192 / 200 / 204 / 224 ms |
| of which CPU in `encode` (walk + sort + build 8M segments + 255 MB `create_buffer_init`) | 310 ms | 172 ms |
| of which submit + GPU (all 8M instances, no culling) + readback | 25 ms (zoom 1), 53 ms (0.037) | same |
| segment build alone (replica) | 167 ms | 108 ms (`pressure_width` in `ink` stays at opt 0) |
| walk 100k `BTreeMap` objects (count strokes) | **7.0 ms** | 2.6 ms |
| collect + sort by z | 8.2 ms | 3.1 ms |
| `Stroke::bounds()` of all strokes | 121 ms | 118 ms (lives in `model`, opt 0) |
| compact encode (u16 + u8 per sample), throughput | 56k samples/ms | 146k samples/ms |
| Douglas–Peucker importance (all levels, one pass), throughput | **4.4k samples/ms** | 29k samples/ms |

About 90 % of today's frame is CPU work that M3 removes. One consequence matters for the design:
in the debug profile, **even one walk over the 100k objects (7 ms) exceeds the frame budget.**
Nothing may be O(objects) or O(samples) per frame. Ingest must be incremental with count budgets,
and change detection must be O(1)/O(changes).

### 0.3 Is the ADR's 1/32-unit grid good enough for the protected goldens?

Every golden case was replayed. Its canvas was quantised the way a compact format would quantise
it, rendered with the current pipeline and compared with the approved PNG using the goldens test's
own rule (channel tolerance 6, at most 0.1 % of pixels):

| Quantisation | loops-4 z1 | ramp-3 z1 | **ramp-3 z3** | tilt-2 z1 | scribble-5 z1 | **scribble-5 z4** | fast-10 z0.5 |
|---|---|---|---|---|---|---|---|
| none (sanity) | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| radius 8-bit only | 0 | 0 | 0 | 0 | 0 | 0 (max diff 4) | 0 |
| **1/32 unit (ADR) + r8** | 0.005 % | 0.0003 % | **0.37 % FAIL** | 0 | 0.003 % | **0.60 % FAIL** | 0 |
| 1/64 + r8 | 0 | 0 | 0.009 % | 0 | 0 | **0.12 % FAIL** | 0 |
| 1/128 + r8 | 0 | 0 | 0 | 0 | 0 | 0.001 % | 0 |
| 1/256 + r8 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

**The format in ADR 0003 decision 4 ("u16 … 1/32 unit within a 2,048-unit chunk") fails two
protected goldens** (zoom 3 and 4). It would also break R-CANVAS-001's AC ("no jitter > 0.1 px at
max zoom"), because at zoom 64 the 1/32 grid is 2 px. The 8-bit radius is fine. Goldens are
protected and must not change, so the coordinate parameter has to change. §1.2 proposes a format
that keeps the 8-byte budget, stays lossless against the model's 1/1024 storage grid, and needs
an owner-approved amendment (D1).

---

## 1. M3.1 — Data layout and GPU resources

### 1.1 Spatial units

- **Cell / chunk:** a square of 2,048 canvas units with key `(cx, cy) = (floor(x/2048),
  floor(y/2048))` as `i64`. Coordinates are bounded by `limits::MAX_COORD = 1e9`, so keys stay
  within ±488,282. The chunk origin is `key·2048` as **f64**, which is exact.
- **Piece:** the part of one stroke that belongs to one cell (§1.3). Most strokes are one piece.
- **Batch:** the unit the GPU draws for a chunk: at most 65,535 runs and 2^20 points. A chunk is
  normally one batch, plus one highlighter batch if it has highlighter strokes.

### 1.2 Point format (8 bytes per sample; lossless) — needs D1

ADR 0003 decision 4 is kept in everything except the coordinate grid: one shared per-stroke point
buffer, no per-segment duplicates, u16 fixed-point coordinates, an 8-bit pressure-derived radius,
and colour/tool by index. The change is that the 16-bit offsets are measured **from a per-run
origin on the model's own 1/1024 grid** instead of from the chunk origin on a 1/32 grid.

```
Point (8 B, two u32 words):
  w0 = dx: i16 | dy: i16        offset from the run origin, units of 1/1024 canvas unit (±32 units)
  w1 = run: u16                 batch-local run index
     | r: u8                    radius / stroke.r_max, 0..255
     | flags: u8                bit0 STROKE_START (draw a dot: prev = self)
                                bit1 PIECE_CONT   (first point of a non-first piece: no instance)
                                bit2 SEAM_DUP     (segment owned by another chunk; skipped when unscissored, §3.1)
Run (16 B):   origin_x: f32, origin_y: f32   (chunk-local; exact, see below)
              stroke: u32                     (batch-local stroke slot)
              _pad: u32
Stroke (16 B): color: u32 (0xRRGGBBAA, highlighter alpha applied)
              r_max: f32  (canvas units; max over samples of the tool's width curve / 2)
              flags: u32  (bit0 DEAD, bit1 HIGHLIGHTER, bits 8..: palette/tool index reserved)
              z_rank: u32 (order inside the batch, for checks/debug)
LOD index:    u32 per kept point (batch-local point index), one list per kept level L3..L6
```

- A new run starts whenever a sample is more than 32 units (32,767/1024) from the current run
  origin. The run origin is that sample, so its offset is (0,0). Measured on the reference note:
  **2.97 runs per stroke** (296,716 runs), which adds about 0.6 B per sample.
- **Exactness:** samples are quantised to 1/1024 unit (`model::grid::XY`), and chunk origins are
  multiples of 2048. A chunk-local coordinate `x − origin` is therefore a multiple of 1/1024. Its
  magnitude is below 2,048 + piece margin, and in every case below 16,384 units, so it fits in
  ≤ 24 mantissa bits. `origin_local + d/1024` is exact in f32. Decoding reproduces the model
  coordinates **exactly** (unit-tested), so there is no grid error at any zoom.
- **Radius:** `r = r_max · q/255`, where `r_max` is the stroke's largest radius from the tool's
  width function (`ink::pressure_width`, later also tilt for the highlighter, R-PEN-003). The error
  is at most `r_max/510`. Measured on the goldens: max channel diff 4, 0 failing pixels.
- **Memory (reference note, estimate from measured counts):** points 7.96M × 8 B = 64 MB, runs
  297k × 16 B = 4.7 MB, strokes 100k × 16 B = 1.6 MB, LOD L3–L6 indices (measured 1.42M + 1.11M
  + 0.34M + 0.20M points) × 4 B = 12 MB, seam duplicates about +5 %. That is **≈ 86 MB of ink**,
  plus the tile atlas (128 MB, §4.2), about 214 MB in total. ADR target ≤ 250 MB. Measure for
  real in M3.1 (`FrameStats.gpu_bytes`).

**Alternative that stays literally inside ADR 0003 (if D1 is declined):** keep 1/32 chunk-local
u16 as the resident format and add an f32 chunk-local "fine" buffer cache for the chunks visible
at zoom > 1. The grid error is ≤ 1/64 unit, which passed all zoom ≤ 1 goldens in §0.3. This needs
two vertex formats, a cache with its own budget and eviction, and a switch at zoom 1 that can
pop. It is more code and more risk for no benefit, so it is not recommended.

### 1.3 Strokes that cross chunk boundaries

- Membership is per segment. Segment `j` (from `p[j−1]` to `p[j]`; for `j = 0` the dot at
  `p[0]`) belongs to **every** cell whose rectangle intersects the segment's bounding box inflated
  by `r_max + M`, with `M = 32` units (seam margin, §3.1). The segment's **owner** is the cell
  that contains `p[j]`. In the other cells its copy is flagged `SEAM_DUP`.
- A piece is a maximal run of consecutive segments of one stroke in one cell. If a piece does not
  start at the stroke's first sample, its first point (`p[j−1]`) carries `PIECE_CONT`. That point
  is only the predecessor and produces no instance, so no extra dot is drawn.
- Pieces longer than 65,535 runs or 2^20 points (hostile or huge strokes) are cut into several
  pieces. The cut point is repeated as the next piece's `PIECE_CONT` predecessor.
- A stroke with all its segments in one cell is one piece. Its instance sequence is exactly
  today's sequence: `(s0,s0), (s0,s1), …, (s[n−2],s[n−1])`. This is why the goldens stay
  unchanged. Today's joints overlap twice and blend twice; that behaviour is kept bit-for-bit in
  structure.
- A chunk's `bounds` is the union of its pieces' inflated bounding boxes. It can exceed the cell
  by `r_max + M` and is used for culling.

### 1.4 GPU resources and allocation

- **Arenas:** `wgpu::Buffer`s of 32 MiB with `STORAGE | COPY_DST | COPY_SRC`. This is below
  `Limits::default()` `max_storage_buffer_binding_size = 128 MiB` and `max_buffer_size = 256 MiB`
  (verified in `wgpu-types-30.0.1/src/limits.rs`; `request_device` uses
  `DeviceDescriptor::default()`, whose `required_limits` is `Limits::default()`). Each arena is
  bound as one `var<storage, read> arena: array<u32>` in **bind group 1**. Storage buffers in the
  vertex stage need `DownlevelFlags::VERTEX_STORAGE`. wgpu-hal's Vulkan adapter always reports it
  (`wgpu-hal-30.0.1/src/vulkan/adapter.rs:689`), and wgpu-core checks it at
  `device/resource.rs:2916`. `max_storage_buffers_per_shader_stage` is 8 by default; the design
  uses 1.
- **Sections:** each batch owns four sub-allocations (strokes, runs, points, LOD lists). They may
  be in different arenas. A batch whose sections are in different arenas is moved into one arena
  on its next rebuild (rare).
- **Allocator:** first-fit with a `BTreeMap<offset, len>` free list per arena, 256-byte
  granularity and coalescing on free. Sections grow by doubling (minimum 4 KiB). Growth = allocate
  a new range, then `CommandEncoder::copy_buffer_to_buffer` (a GPU-side copy; no CPU mirror is
  kept), then free the old range.
- **Deferred free:** a freed range goes to `pending_free[frame]` and is reused only after the
  **next** submit. `queue.write_buffer` data is applied at submit before the frame's command
  buffers, so reusing a range in the same frame could overwrite the source of a recorded copy.
- **Memory cap:** `max_arena_bytes` defaults to 512 MiB. When an allocation would exceed it, the
  least-recently-visible non-visible chunks are evicted. They are re-ingested when they become
  visible again. This keeps memory bounded on hostile canvases (R-PERF-007).
- **Per-draw parameters:** one uniform buffer per frame with `has_dynamic_offset: true`. It holds
  one 256-byte slot per draw (`min_uniform_buffer_offset_alignment = 256`, default limit):
  ```
  struct Draw { off_px: vec2<f32>, zoom: f32, min_r: f32, size: vec2<f32>,
                base_stroke: u32, base_run: u32, base_point: u32, base_lod: u32,
                count: u32, mode: u32 /* bit0 use LOD list, bit1 scissored */ }
  ```
  `off_px = f32((chunk_origin − vp.origin) · zoom)` is computed in **f64 on the CPU** per chunk.
  This keeps R-CANVAS-001: the f32 part is chunk-local and small. Instance `i` reads
  `base + i`, so the design does not depend on whether `instance_index` includes the draw's first
  instance.
- **Vertex shader:** for instance `i` it reads point `k` (`k = i`, or `lod[i]` in LOD mode) and
  its predecessor `k−1` (or `lod[i−1]`). If `STROKE_START` is set, the predecessor is `k` itself.
  `PIECE_CONT`, `DEAD` and (when unscissored) `SEAM_DUP` produce a degenerate quad. It decodes
  `pos = run.origin + d/1024` and `px = off_px + pos·zoom`, and gets `r` from the stroke table.
  The fragment shader is today's capsule SDF, unchanged.
- **Upload:** encoded bytes are staged in CPU vectors and written with `queue.write_buffer`. At
  most `upload_bytes` are written per frame (measured cost: 4 MB ≈ 0.29 ms). A batch draws only
  its fully uploaded prefix (`count`). Ingest pauses while more than 2 × `upload_bytes` are staged.
- **Device loss:** `take_fault() == Lost` drops all arenas and resets ingest. Content comes back
  through the normal budgeted ingest, so it never crashes and memory stays bounded.

### 1.5 Precision summary (R-CANVAS-001)

The f64 chunk origin minus the f64 viewport origin is computed once per chunk on the CPU.
Everything on the GPU is chunk-local. At zoom 64 a visible chunk's `off_px` is at most about
1.3e5 px, where the f32 ulp is 0.008 px. That is below the 0.1 px AC. The existing tests
`far_from_origin_renders_identically` and `r_canvas_001_*` use integer offsets of 1e7, and they
stay exact because the quantisation is translation-invariant.

---

## 2. Keeping GPU state across frames with the current API

### 2.1 Constraints found in the code

- `Canvas` is a pub struct with a single pub field. It is built as a **struct literal**
  (`Canvas { objects: objs }`) in `tests/acceptance/src/lib.rs`, and the same file builds
  `CanvasObject { id, z, kind }` and `Stroke { … }` as literals. **No field can be added to
  `Canvas`, `CanvasObject` or `Stroke`** without editing protected acceptance code. A revision
  counter cannot live in the canvas.
- Canvases are mutated directly outside `Workspace::apply`: `storage::load`,
  `engine::Session::merge` (lazy pages, `c.objects.entry(..).or_insert`) and tests
  (`c.objects.insert`).
- `r_perf_002` passes the same `&Canvas` five times with no changes. The goldens reuse one
  `Headless` for 7 different canvases. `numeric.rs::stroke_width_follows_pressure_curve` renders
  four different one-stroke canvases, possibly at the same stack address, that differ **only in
  pressure**. Whatever detects changes must catch that.

### 2.2 Two sync modes

```rust
pub struct Scene<'a> {
    pub canvas: &'a Canvas,
    /// Change journal of `canvas` (engine-maintained). None = untracked (tools, tests, CLI).
    pub changes: Option<&'a vibenote_model::ChangeLog>,
    pub live: &'a [&'a Stroke],
    pub viewport: Viewport,
}
```

**Tracked (the app).** The engine keeps one `ChangeLog` per loaded note (§6.2). The renderer
remembers `(epoch, rev)` from the last frame:
- Epoch differs: this is a different canvas instance, so reset (drop the batches and start ingest).
- `log.since(rev)` = `Current`: nothing to do (O(1)).
- `Changed(ids)`: put the ids on the **edit queue**, deduplicated (O(changes)).
- `TooOld` (the ring overflowed, for example after a huge merge): run a **verify pass** (below).

**Untracked (`Headless::render`, CLI, tests).** The renderer computes a key:
- **Small canvas** (`objects.len() ≤ 4,096` and at most 65,536 samples, with an early exit while
  walking): the key is a 64-bit FNV-1a hash over all render-relevant fields (id, z, kind tag, tool,
  colour, width, every sample's x/y/pressure/tilt bits). This costs at most about 1 ms at opt 0 and
  is exact for every existing test. If the key changed, the renderer resets and re-ingests in the
  same frame (small canvases always fit the budget, §2.4).
- **Large canvas:** `key = (address of the Canvas, objects.len(), fp(first object), fp(last
  object))`. Each fingerprint is O(1): id, z, tool, colour, width, sample count, first/middle/last
  sample and `samples.as_ptr()`. The key itself costs O(log n). If the key changed, the renderer
  runs a **verify pass**. Contract (documented on `render`): *a large untracked canvas whose key is
  unchanged is assumed unchanged*. Callers that mutate a large canvas in place call
  `invalidate()` or use the tracked mode. No current caller does this: the CLI renders once, and
  the perf test does not mutate.

**Verify pass (budgeted):** the bulk cursor walks every object again. It compares each object's
O(1) fingerprint with the renderer's record, re-encodes the ones that differ, and at the end drops
records it did not visit. GPU data that is still valid stays drawn, so nothing flickers.

### 2.3 Renderer state

```
CanvasState {
  sync: Sync { epoch, rev } | Untracked { key },
  records: HashMap<ObjectId, Record { fp: u64, pieces: SmallVec<[(ChunkKey, BatchId, slot); 1]> }>,
  chunks: HashMap<ChunkKey, Chunk>,
  edit_queue: VecDeque<ObjectId> (+ HashSet for dedup),
  bulk: Option<Cursor { after: Option<ObjectId>, verify: bool }>,
  staged: Vec<Upload>, staged_bytes,
  tiles: TileCache (§4.2), lod: LodWorker handle (§4.1),
}
Chunk { key, origin: (f64,f64), bounds: Rect, batches: Vec<Batch>, hl_batches: Vec<Batch>,
        order_ok: bool, dead_points: u32, live_points: u32, last_visible_frame: u64, gen: u64 }
```

### 2.4 Per-frame work (deterministic count budgets; no wall clock)

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameBudget {
    pub walk_objects: u32,     // bulk cursor steps per frame
    pub ingest_samples: u32,   // samples encoded per frame (bulk)
    pub edit_samples: u32,     // hard cap for edit-queue work per frame (hostile bound)
    pub upload_bytes: u64,     // queue.write_buffer bytes per frame
    pub tile_fills: u32,       // ADR: 8–16
    pub crossfade_frames: u32, // tile fade-in length in frames
}
```

Defaults: `FrameBudget::HEADLESS = { walk 4096, ingest 32_768, edit 1_000_000, upload 4 MiB,
tiles 8, crossfade 8 }`. `FrameBudget::APP` raises `ingest` to 262,144 for the release build;
tune it with `cargo xtask bench`. `UNLIMITED` is for `render_settled`.

These are counts, so the result depends only on inputs (architecture rule 6). The HEADLESS
numbers are sized from §0.2 at **opt-level 0**. Walking 4,096 objects takes about 0.3 ms, and
encoding 32k samples takes about 0.6 ms, or 1–1.5 ms with piece splitting and runs. Together with
the warmed readback (≤ 2.6 ms) and the draw, frame 1 stays at about 4.5 ms or less.

Order of work in `encode`, all O(budget + visible chunks):
1. **Sync:** compute the key or read the journal (O(1) / O(changes)).
2. **Edit queue first:** for each id, drop the old record (set `DEAD` in its stroke-table entries,
   which is a 16 B write) and, if the object exists, encode it. Strokes are appended to the batch
   when `(z, id)` is above the batch's last entry. Otherwise `order_ok = false`, and the chunk is
   rebuilt from the model in (z, id) order, in the same frame for edits (one chunk ≈ 56k samples
   ≈ 1 ms at opt 0). Edits never wait for bulk work.
3. **Bulk:** `canvas.objects.range((Excluded(after), Unbounded))` takes up to `walk_objects`
   entries (O(log n) to start). Each batch of walked strokes is sorted by (z, id) and encoded until
   `ingest_samples` is reached. Non-stroke objects are skipped until their milestones. When the
   cursor reaches the end, every chunk with `order_ok == false` is queued for a rebuild (bulk
   priority). Batches with `dead_points > 25 %` are compacted by a rebuild.
4. **Uploads:** up to `upload_bytes`.
5. **Hand-off to LOD:** batches that are new or rebuilt are sent to the LOD worker (§4.1).
6. **Cull:** visible chunks = chunks whose `bounds` intersect the viewport rectangle (in f64).
   They are looked up through the chunk key range of the viewport rectangle, expanded by the
   largest chunk overhang. With few chunks overall (≤ ~200), a linear scan of the chunk map is
   also fine.
7. **Mode:** if `zoom < 0.25` or visible instances > 500,000 (summed from per-batch counts), use
   tiles (§4.2); otherwise draw directly (§3).
8. **Live/pending:** live strokes, plus edit-queue strokes not yet uploaded, are drawn by today's
   per-segment f32 path (`Seg`, rebuilt each frame, small).

Large edits are bounded. A merge page or a huge paste goes to the edit queue but is handled at
most `edit_samples` per frame. The overflow stays queued and is drawn through the pending path
only while it is ≤ 64k samples; above that it simply appears over the next frames.

### 2.5 What the first frames of a fresh renderer draw (the `r_perf_002` sequence)

Fresh `Headless` (pipelines and a 256² frame warmed up in `new`), with the 100k-stroke canvas,
untracked and large:
- **Frame 1** (zoom 1, origin 0): key differs, so start bulk ingest. 4,096 objects are walked and
  32k samples encoded (≈ 400 strokes in id order, here the top of page 0) and uploaded (≈ 0.3 MB).
  Chunk (0,0) is visible and drawn. Then readback. Expected ≈ 2.5 + 1.5 ms.
- **Frames 2–3** (zoom 1 at x = 37; zoom 0.5): about 400 more strokes each. Direct draw at L0.
- **Frame 4** (zoom 0.1): tile mode at level ℓ = −3 (tile zoom 0.125). There, one 256 px tile is
  exactly one 2,048-unit chunk. Non-empty visible tiles (1–2 so far) are filled within the budget
  of 8. Tiles over empty canvas are marked empty without rendering. Missing tiles fall back as in
  §4.2.
- **Frame 5** (zoom 0.037): tiles at ℓ = −4. Same pattern.

Every frame does bounded work, so each call should be ≤ ~4.5 ms at opt 0. The content is
incomplete, which the risk note explicitly allows: completeness is checked by the goldens and by
`render_settled` tests, not by this test. In the app (tracked, lazy open), the engine delivers the
viewport's objects first (M2.3). Visible content is therefore ingested first, and the
APP-budget release build should finish the reference note's L0 in roughly 30 frames. Measure
this in M3.3 with `xtask bench`.

### 2.6 `opt-level` for the render crate in the dev profile

Measured (§0.2): render at opt-level 2 makes CPU loops 2.5–3× faster (object walk 7.0 → 2.6 ms,
encode 56k → 146k samples/ms, DP 4.4k → 29k samples/ms). This follows the storage precedent.
**Recommendation:** the design does **not** depend on it, because the budgets are sized for
opt 0. Adding
```toml
[profile.dev.package.vibenote-render]
opt-level = 2
[profile.dev.package.vibenote-ink]
opt-level = 2
```
gives the debug-profile timing test more margin and makes debug apps usable on large notes. It
does not change any test or budget, but the owner should know that it was done (D3).

---

## 3. Draw order

### 3.1 Z-order across chunks: scissor partition

- Within a batch, strokes are stored in (z, id) order. That is today's order, so the goldens stay
  unchanged.
- **Across chunks:** each chunk is drawn with a scissor rectangle equal to its cell in pixels.
  Pixel ownership is decided by pixel centre:
  `x0_px = ceil((cell_x0 − vp.x)·zoom − 0.5)`, computed in f64 and clamped to the target. Adjacent
  cells therefore **partition** the pixels exactly. Because membership (§1.3) includes every
  segment whose drawn footprint can reach the cell, each pixel is drawn by exactly one chunk, and
  that chunk has every stroke that touches the pixel in correct z order. The cross-chunk order is
  exact, and highlighter layers are not double-counted at seams.
- The footprint condition is `r_max + (min_r + 0.5 px)/zoom ≤ r_max + M` with `M = 32` units and
  `min_r = 0.35 px`. It holds for `zoom ≥ 0.85/32 ≈ 0.027`. Below that zoom (deep overview, every
  stroke a hairline under 1 px), chunks are drawn **unscissored**, and `SEAM_DUP` copies are
  skipped so each segment is drawn once. Cross-chunk z order can then be off only between
  sub-pixel strokes. Overlapping opaque ink of the same colour gives the same result in either
  order (premultiplied "over" with one colour is commutative), so the effect is invisible in
  practice.
- Later object kinds (PDF, image, text; M4.8/M6/M7) become additional z-ordered runs inside a
  chunk's draw list. The scissor partition carries over.

### 3.2 Highlighter (ADR 0003 open item; R-PEN-005 "drawn below ink of the same layer")

Frame order: paper → **highlighter pass** → **pen pass** → live strokes.
- The highlighter pass draws each visible chunk's highlighter batch (scissored) into a
  viewport-sized offscreen layer (RGBA8, cached per size). Blending uses `BlendOperation::Max` for
  colour and alpha (`wgpu-types-30.0.1/src/render.rs`, `Max = 4`). It then composites the layer
  over the paper with premultiplied "over" (one full-screen triangle). A stroke's own overlapping
  joints and other overlapping highlighter strokes of the same colour no longer double-blend, so
  the colour stays uniform. A live highlighter stroke goes into the same layer, after the committed
  ones.
- This is a per-**layer** coverage pass, a cheaper generalisation of the ADR's per-stroke pass:
  one offscreen pass per frame or tile instead of one per stroke. The behavioural difference:
  where differently coloured highlighters overlap, the result is the per-channel maximum (lighter)
  instead of a mix. That is a product choice (D2). No golden uses the highlighter. `r_pen_003`
  measures coverage in the middle of a straight 2-sample stroke, which is unaffected.

### 3.3 Live stroke

Live strokes (and pending edits, §2.4) are drawn last with today's `Seg` path, unscissored and
never tiled (ADR 0003 decision 3). The live highlighter goes into the highlighter layer (§3.2).
`ui::live_stroke` currently passes only pen strokes; the ui follow-up should pass highlighter
strokes too.

---

## 4. M3.2 LOD and M3.3 tile cache

### 4.1 LOD (M3.2): L0 + L3…L6, ~1 px tolerance

- **Algorithm (in `vibenote-ink`, which owns stroke geometry per architecture §1):**
  Douglas–Peucker *importance* in a single pass per piece. It uses an explicit stack (no
  recursion, so it is bounded for 1M samples), and the split distance is clamped to the parent's
  (`d_child = min(d_child, d_parent)`). This makes the levels nested, and level L is "endpoints +
  points with importance > tol_L", where `tol_L = 0.5·2^L` units. The bound holds: the parent's
  split distance is the maximum over its whole span, so every dropped point is within `tol_L` of
  its chord. The distance includes the radius term (`|r_i − lerp(r)|`), so widths do not jump.
  Measured counts on the reference note (xy only): L3 1.42M, L4 1.11M, L5 0.34M, L6 0.20M points.
  This confirms the ADR's choice to keep only L0 and L≥3.
  ```rust
  // crates/ink/src/lod.rs
  pub const LOD_LEVELS: [u8; 4] = [3, 4, 5, 6];
  /// Importance per point (f32, canvas units; endpoints = +inf). Points are (x, y, r) in units.
  pub fn importance(points: &[[f64; 3]], out: &mut Vec<f32>) -> Result<(), InkError>;
  /// Indices of points kept at `level` (nested: level+1 ⊆ level), endpoints always kept.
  pub fn kept(importance: &[f32], level: u8, out: &mut Vec<u32>);
  pub fn tolerance(level: u8) -> f64; // 0.5 * 2^level
  ```
- **Level choice:** the kept level L is the smallest level in {0, 3, 4, 5, 6} with
  `tol_L · zoom_draw ≤ 1 px`, the ADR's "bias 1". Direct draws at zoom ≥ 0.25 always use L0.
  Tile fills at ℓ = −2 / −3 / −4 / ≤ −5 use L3 / L4 / L5 / L6.
- **Where and on which thread:** DP costs 4.4k samples/ms at opt 0 and 29k at opt 2 (§0.2). Even
  in release, the reference note is about 200 ms of CPU, so it must **not** run on the
  input/render thread. The render crate owns **one LOD worker thread** (`std::thread`, created in
  `Renderer::new`, shut down and joined in `Drop`, never panicking):
  - Input: an owned snapshot of the newly encoded batch's points (8 B each, decoded to
    `(x, y, r)` on the worker) plus `(chunk, batch, gen)`.
  - Output: the index lists per level. The render thread uploads them within `upload_bytes` and
    drops results whose `gen` is stale (architecture rule 5).
  - Triggers are commit (edit queue) and load (bulk ingest). Both are off the pen path: the
    worker never touches the input thread, and pen-down handling waits on nothing.
  - Until a batch's LOD is ready, tile fills draw it at L0 (correct, and only the GPU cost is
    higher). Results do not depend on timing except *which* level a transient frame uses. Goldens
    (zoom ≥ 0.5) only use L0. `Headless::render_settled` waits for the worker before its final
    frame.

### 4.2 Tile cache (M3.3)

- **When:** `zoom < 0.25` or visible instances > 500,000. Tile level `ℓ = ceil(log2(zoom))`, tile
  zoom `2^ℓ ∈ [zoom, 2·zoom)`, so a tile is shown at scale 0.5–1 (minified, never magnified).
  Tile key `(ℓ: i8, tx: i64, ty: i64)`. The tile covers canvas `[tx·256/2^ℓ, (tx+1)·256/2^ℓ)` in x
  and the same in y, with f64 origins.
- **Atlas:** a 2D array texture of 4096² RGBA8 with `TEXTURE_BINDING | RENDER_ATTACHMENT`, 2
  layers by default = 512 tiles = 128 MiB (default limits: 8192², 256 layers). Allocation is LRU
  over tile slots. Tiles hold ink only, premultiplied over **transparent**, and are composited over
  paper. Because "over" is associative, the result equals direct rendering, and backgrounds
  (R-CANVAS-006) stay a separate layer.
- **Fill:** at most `tile_fills` (8; the ADR allows 8–16) per frame. A fill renders into the
  atlas slot with viewport and scissor set to the slot. It runs the highlighter and pen passes for
  the chunks that intersect the tile, at the tile's LOD level, with scissor partition inside the
  tile. A tile with no intersecting chunk is marked empty without rendering, and it does not count
  against the budget. Priority: missing visible tiles nearest the viewport centre, then stale
  visible tiles, then the prefetch ring one tile beyond the viewport.
- **Missing tiles:** first a cached coarser tile (ℓ−1, ℓ−2, …) is used for that region
  (magnified, a little blurry). If there is none, the four cached finer tiles (ℓ+1) are used. If
  neither exists, the visible chunks are drawn directly (method A) at one level coarser than the
  tile LOD, scissored to the missing tile rectangles. This is the ADR's "A at a higher LOD bias".
- **Crossfade:** a tile that becomes ready while its area was shown through a fallback fades in
  over `crossfade_frames` (8) frames. The fallback is drawn below during the fade. Fades count
  frames, not wall time, so they are deterministic.
- **Invalidation on commit:** any change to a chunk (append, dead flag, rebuild) has a canvas
  bounding box: the stroke's inflated bounds. All cached tiles at all levels that intersect it
  become **stale** (generation bump). Stale tiles stay drawable and are refilled with edit
  priority, ahead of other fills, so a commit at low zoom appears within one frame. In the frame
  of the commit, added strokes are also drawn directly on top as "pending", like live ink, so they
  never disappear. A removal shows at the latest after the refill in the next frame.
- **Live ink is never tiled** (§3.3).

---

## 5. Interfaces (Rust)

### 5.1 `vibenote-render` (depends on geom, model, ink only; `xtask layers` unchanged)

```rust
pub struct Scene<'a> { pub canvas: &'a Canvas, pub changes: Option<&'a ChangeLog>,
                       pub live: &'a [&'a Stroke], pub viewport: Viewport }
pub struct FrameBudget { /* §2.4 */ }
impl FrameBudget { pub const HEADLESS: Self; pub const APP: Self; pub const UNLIMITED: Self; }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrawMode { #[default] Direct, Tiles }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub complete: bool,          // ingest + uploads + LOD + visible tiles all done
    pub mode: DrawMode,
    pub ingested_samples: u64, pub pending_objects: u64, pub edits_pending: u64,
    pub chunks_visible: u32, pub instances: u64, pub lod_pending: u32,
    pub tiles_drawn: u32, pub tiles_filled: u32, pub tiles_missing: u32,
    pub upload_bytes: u64, pub gpu_bytes: u64,
}

impl Renderer {
    pub fn new(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self;
    /// Creates every pipeline for `format` (ink, highlighter layer, composite, tile) up front.
    pub fn warm_up(&mut self, format: wgpu::TextureFormat);
    pub fn set_budget(&mut self, budget: FrameBudget);
    /// Records one frame. Does bounded work (§2.4). The caller submits.
    pub fn encode_scene(&mut self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView,
                        format: wgpu::TextureFormat, scene: &Scene<'_>)
                        -> Result<FrameStats, RenderError>;
    /// Kept for compatibility: untracked scene.
    pub fn encode(&mut self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView,
                  format: wgpu::TextureFormat, canvas: &Canvas, live: &[&Stroke], vp: &Viewport)
                  -> Result<FrameStats, RenderError>;
    /// Call after `queue.submit` of the frame (releases deferred frees, collects LOD results).
    pub fn frame_submitted(&mut self);
    /// Forget all cached canvas state (next frame starts ingest from scratch).
    pub fn invalidate(&mut self);
    pub fn take_fault(&self) -> Option<GpuFault>; // unchanged
}

impl Headless {
    /// Also warms up: pipelines for HEADLESS_FORMAT + one 256×256 frame with readback.
    pub fn new() -> Result<Self, RenderError>;
    /// Unchanged signature: one budgeted frame of an untracked scene (FrameBudget::HEADLESS).
    pub fn render(&mut self, canvas: &Canvas, live: &[&Stroke], vp: &Viewport) -> Result<Image, RenderError>;
    pub fn render_scene(&mut self, scene: &Scene<'_>) -> Result<(Image, FrameStats), RenderError>;
    /// Renders frames until `FrameStats::complete` (or `max_frames`, then Err), returns the last.
    /// For the CLI (`render-png`, `ui.screenshot`) and completeness tests of large canvases.
    pub fn render_settled(&mut self, canvas: &Canvas, live: &[&Stroke], vp: &Viewport,
                          max_frames: u32) -> Result<Image, RenderError>;
    pub fn set_budget(&mut self, budget: FrameBudget);
    pub fn invalidate(&mut self);
}
```

Internal modules: `chunk.rs` (cells, pieces, encode/decode, batches), `arena.rs` (allocator,
deferred free), `sync.rs` (keys, fingerprints, journal sync, cursor), `lod_worker.rs`,
`tiles.rs`, `pipeline.rs` (passes, shaders), `headless.rs` (cached target + readback; a single
memcpy when `bytes_per_row == 4·width`, which holds for 1920).

### 5.2 `vibenote-model`: `ChangeLog` (new, standalone; `Canvas` is untouched)

```rust
/// Per-canvas change journal kept by the owner of the canvas (the engine). Bounded.
pub struct ChangeLog { epoch: u64, rev: u64, floor: u64, entries: VecDeque<(u64, ObjectId)> }
pub enum Since<'a> { Current, Changed(Box<dyn Iterator<Item = ObjectId> + 'a>), TooOld }
impl ChangeLog {
    pub const CAPACITY: usize = 65_536;
    pub fn new(epoch: u64) -> Self;
    pub fn epoch(&self) -> u64;
    pub fn rev(&self) -> u64;
    /// Records that object `id` was inserted, replaced or removed (rev += 1).
    pub fn record(&mut self, id: ObjectId);
    pub fn since(&self, rev: u64) -> Since<'_>;
}
```

It lives in `model` because it is plain data about the model. `render` (consumer) and `engine`
(producer) both already depend on `model`, and `hwr` can use it later for "blocks touched since".

### 5.3 `vibenote-engine` (M3.1b)

- `Session` gains `logs: BTreeMap<NoteId, ChangeLog>` and `next_epoch: u64`, which is
  deterministic.
- `pub fn change_log(&self, note: NoteId) -> Option<&ChangeLog>`.
- Recording happens at the existing choke points: after every successful `ws.apply_all` (in
  `Session::commit`, `Session::apply_permanent` and `Session::step_back`, which is undo/redo),
  `record(id)` runs for
  `InsertObject`/`RemoveObject`/`ReplaceObject`, and `CreateNote`/`DeleteNote` reset the note's
  log with a new epoch. `merge` records each inserted id. Loading a note creates its log.

### 5.4 `vibenote-ui` (M3.1b)

`Gpu::draw` and the shm fallback build a `Scene` with `changes:
session.change_log(note)`. They use `FrameBudget::APP` and call `frame_submitted()` after
`queue.submit`. `ui.screenshot` (when it lands) uses `render_settled`.

### 5.5 CLI

`apps/vibenote-cli/src/headless.rs::render_note` and the `render-png` command call
`render_settled`. For the goldens' small canvases this is the same single frame as `render`, and
for large notes the PNG is complete. `goldens.rs` itself is unchanged.

---

## 6. Plan of work and tests

### 6.1 M3.1 (render): GPU-resident chunks

Implementation order: encode/decode + allocator (pure CPU, unit-tested) → shader + direct draw of
a fully ingested canvas (goldens green) → sync modes + budgets + culling → highlighter layer.

Tests (new ones in `crates/render/tests/` and unit tests; nothing protected is touched):
- **Lossless encode:** proptest (add `proptest` as a render dev-dependency; MIT/Apache) over
  random strokes, including at ±1e7 and ±1e9, across seams, with 1 sample, with 1M samples and
  with width 9,999. Decoding gives exactly the quantised model coordinates. Radius error ≤
  `r_max/510`. Pieces cover every segment exactly once as owner. Run/piece limits hold.
- **Oracle equivalence:** the old per-segment path stays as a `pub(crate)` test oracle. Random
  small canvases rendered both ways must match within the goldens' rule (tolerance 6, ≤ 0.1 %).
- **Seam z-order:** strokes of two colours overlapping across a cell seam, with z order opposite
  to id order. The overlap pixel has the top stroke's colour, as in the oracle, at zoom 1 and 4.
- **Incremental correctness:** for untracked small canvases, A then a mutated A′ (pressure only,
  as in `numeric.rs`) equals a fresh render of A′. For tracked canvases, sequences of inserts,
  removes, replaces, z changes, undo-restored low z and a `TooOld` overflow, after
  `render_settled`, equal a fresh render (pixel-exact, same pipeline).
- **Budget compliance:** a synthetic 20k-stroke canvas under `HEADLESS`: every frame's
  `FrameStats` stays within the budget, `complete` arrives after the expected number of frames,
  and the settled image equals the image with `UNLIMITED`.
- **Culling:** off-screen chunks are not drawn (`chunks_visible`), and the image is unchanged.
- **Allocator:** a proptest of first-fit, coalescing, deferred free and no overlap. Memory-cap
  eviction keeps `gpu_bytes ≤ cap`.
- **Hostile:** no panic and bounded `gpu_bytes` for a 1M-sample stroke, extreme widths and
  coordinates ±1e9. `invalidate()` then a re-ingest gives the same image.
- **Existing tests that must stay green, unchanged:** `apps/vibenote-cli/tests/goldens.rs` (all 7
  goldens: loops-4, pressure-ramp-3, pressure-ramp-3-zoom3, tilt-sweep-2, scribble-5,
  scribble-5-zoom4, fast-10-zoom0.5), `crates/render/tests/numeric.rs`
  (`far_from_origin_renders_identically`, `stroke_width_follows_pressure_curve`), acceptance
  `r_canvas_001_infinite_canvas_precision`, `r_pen_002_*` (rendered width), `r_pen_003_*` (tilt;
  uses the highlighter), and the CLI `render-png` tests.
- **Bench (not a test):** extend `cargo xtask bench` with the ADR scenarios (`page_pan`,
  `page_fling`, `deep`, `overview`, `zoom_sweep`) on the compact format, using GPU timestamps as
  in `spikes/render`. ADR 0003 decision 4 asks for this comparison against the baseline.

### 6.2 M3.1b (proposed new row; model, engine, ui): change feed

`ChangeLog` + engine recording + ui `Scene`. Tests:
- A property test in the engine: random commands, undo/redo, lazy pumps and note re-creation.
  Replaying `since()` deltas onto a mirror of `(id → object)` always reproduces the canvas, or
  `TooOld` is returned.
- Ring overflow gives `TooOld`. Epochs are deterministic.
- App smoke [sim] with the reference note: erase or commit while zoomed out shows the change in
  the next frame (`FrameStats.edits_pending == 0`).

### 6.3 M3.2 (ink, render): LOD

- `ink::lod` numeric tests: for each level, the Hausdorff distance between the kept polyline and
  the full polyline, and the radius error, are ≤ `tol_L` (proptest, including hostile 1M-sample
  strokes, collinear points and duplicates). Levels are nested. Endpoints are kept. Output is
  deterministic. Worst-case work and memory are bounded (no recursion).
- Render: `level_for(zoom)` keeps `tol·zoom ≤ 1 px`. A settled render at zoom 0.2 at L3 differs
  from the L0 render by ≤ 1 px of edge displacement (numeric edge test). The LOD worker drops
  stale generations and shuts down cleanly (`Drop`).

### 6.4 M3.3 (render): tiles

- A settled tile composite at a tile zoom (scale 1) equals the direct render within tolerance 6
  (≤ 0.1 %). At scale 0.6 it is within a documented resampling tolerance (a new numeric test, not
  a golden).
- Per-frame `tiles_filled ≤ budget`. Fallback order: coarser, then finer, then direct.
  Crossfade lasts exactly N frames.
- Commit at zoom 0.1: the new stroke is visible in the same frame (pending overlay) and in the
  refilled tile in the next frame. Erase: gone after ≤ 1 frame.
- The live stroke is visible in frame 1 at zoom 0.1 with an empty tile cache.
- **`r_perf_002_frame_time_reference_note` turns green.** Parity +R-PERF-002, then
  `xtask parity --raise`.
- Goldens: no tile proposals in M3. Zoomed-out goldens (e.g. overview of a fixture at zoom 0.1
  through `render_settled`) may be **proposed** to the owner later.

---

## 7. Risks

1. **The debug-profile first frame is tight:** about 2.0–2.6 ms of readback/warm-up variance plus
   ingest. Mitigations: the count budgets are tunable constants (not protected), the D3 opt-level
   option, and the warm-up. The acceptance perf tests run in parallel threads of one test binary
   (other perf tests load the 100k-note workspace at the same time), so CPU contention can cause
   outliers. Watch this in `xtask parity`, and do not loosen the test.
2. **Vertex shader cost:** fetching 2 points + 2 runs + 1 stroke per instance is more ALU/fetch
   than the spike's 32 B instance. ADR page view baseline: 0.31 ms. Expect ≤ 2×. Benchmark (§6.1).
3. **Untracked large canvases:** the contract in §2.2 has a hole for in-place mid-canvas mutation
   without `invalidate()`. No caller does this. The app uses the tracked mode, which has no hole
   (its completeness is property-tested).
4. **Seam duplicates on very dense notes or huge strokes:** a memory overhead estimated at about
   5 %; measure it. Huge-width strokes (up to 10,000 units) inflate membership; they are bounded
   by the piece limits and the memory cap.
5. **Highlighter semantics** (D2) can change visible output later. No golden depends on it yet.
6. **LOD worker timing** makes transient low-zoom frames timing-dependent. All tests that check
   pixels at low zoom use `render_settled`.
7. **Ingest on the input/render thread in the app:** bulk ingest takes a fixed budget per frame.
   If app frame time while loading exceeds 5.5 ms, lower `APP.ingest_samples` while the pen is in
   proximity. The app already pauses page merges then (M2 review M3). The next step would be moving
   L0 encode to a worker, which needs owned snapshots: `Arc` samples in the model (a model change,
   not part of M3).
8. **Atlas memory:** 128 MiB on an APU shares system RAM. Check R-PERF-007 (1.5 GB RSS) with the
   reference note open after M3.3.

---

## 8. Mapping to the execution plan

| Task | Crates | Turns green / proves |
|---|---|---|
| M3.1 | render | new render tests (§6.1); goldens and existing numeric/acceptance render tests unchanged |
| M3.1b (new) | model, engine, ui | change-feed property test; app smoke |
| M3.2 | ink, render | LOD numeric tests |
| M3.3 | render (+ CLI `render_settled`) | tile tests; `r_perf_002_frame_time_reference_note` |

---

## 9. Owner decisions needed

- **D1 (blocking M3.1, locked ADR 0003 decision 4):** change the compact coordinate format from
  "u16, 1/32 unit, chunk-relative" to "i16 offsets on the model's 1/1024 grid relative to
  per-run f32 chunk-local origins (+16 B per run, ~3 runs/stroke)". Evidence §0.3: the ADR grid
  fails 2 protected goldens (0.37 % and 0.60 % vs the 0.1 % limit) and the R-CANVAS-001 0.1 px AC
  at max zoom. The proposed format is lossless and still about 8.6 B/sample. A change proposal
  must be filed as `docs/adr/0011-compact-ink-format.md` (Proposed) from §0.3 and §1.2. The
  alternative inside the ADR (1/32 + an f32 fine cache at zoom > 1) is costed in §1.2.
- **D2 (product, R-PEN-005 [proposal]):** highlighter blending. Overlapping highlighter strokes
  combine by per-channel max within a layer: same colour does not darken, different colours give
  a lighter mix. The alternative, per-stroke layers, costs one offscreen pass per highlighter
  stroke.
- **D3 (build, FYI):** add `[profile.dev.package.vibenote-render]` and `vibenote-ink`
  `opt-level = 2` (storage precedent). Not required by the design.
- **D4 (plan, FYI):** add row M3.1b (change feed in model/engine/ui) to `plan/execution-plan.md`.
  Also, `Headless::new` now warms up, so the first-frame cost moves out of the timed `render`
  call; the measured reason is in §0.1.
