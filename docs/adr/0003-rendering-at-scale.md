# ADR 0003 — Rendering at scale

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved. Decision 4's
  coordinate format amended by ADR 0011 (owner, 2026-10-13). Decision 2 (tile cache) **deferred**
  by the owner on 2026-10-13 ("Defer tiles"), see "Update 2026-10-13" at the end.
- **Date:** 2026-10-11
- **Builds on:** ADR 0001 (locked wgpu renderer), ADR 0002 (UI overlay).
- **Evidence:** `spikes/render/` (`bench`: strategies A/B; `vbench`: vello). Headless,
  offscreen 1920×1200 RGBA8 on the reference GPU (AMD 860M, RADV, Mesa 26.2.4). GPU time from
  wgpu timestamp queries (start/end of each frame's command buffer); 300 frames per scenario.
  Images: `spikes/render/evidence/`. All **[sim]** in the sense of synthetic data on the real GPU.

## Workload
Synthetic note close to R-PERF-000: **100,000 strokes, 7,956,597 samples** (40–120 per stroke,
avg 80), cursive-like loops with pressure-varying width, on ~190 page-sized blocks
(1,400×2,000 units) in a 14-column grid (≈ 22,400 × 30,800 units).
Scenarios: `page_pan` (zoom 1, 8 px/frame), `page_fling` (zoom 1, 60 px/frame), `deep`
(zoom 16), `overview` (whole note in view, zoom ≈ 0.037), `zoom_sweep` (1 → overview → 1 over
300 frames). Frame budget: 5.555 ms (180 Hz).

## Strategies
- **A, direct:** one GPU instance per segment (p0, p1, r0, r1, chunk, colour; 32 B), drawn as an
  oriented quad. The fragment shader computes an anti-aliased capsule SDF with interpolated
  radius. Strokes are binned into 2,048-unit **chunks**. Segment coordinates are f32 relative to
  the chunk origin, and chunk origins are f64 on the CPU, so there is no precision loss far from
  the origin. Chunks are culled by bounding box. **LOD:** per-stroke decimation levels L0…L6
  (tolerance 0.5·2^L units); level chosen from zoom (+ bias).
- **B, tile cache:** 256 px tiles per power-of-two zoom level, rendered with A into an atlas
  (4 × 4096² pages, 1,024 tiles), LRU, at most *N* tile renders per frame, nearest-first. Missing
  tiles fall back to a cached coarser level. Composite = one textured quad per visible tile.
- **C, vello 0.11** (compute rasteriser): scene re-encoded per frame, culled by precomputed
  bounding boxes. **Constant-width** strokes only, so this is a lower bound on its cost; real ink
  would need outline fills.

## Results

GPU ms per frame, p50 / p95 / p99 (max); "over" = frames > 5.555 ms of 300.

| Scenario | A (LOD bias 0) | A (bias 1, ~1 px tol.) | A (bias 2, ~2 px tol.) | B (budget 8 tiles/frame) | B (budget 32) |
|---|---|---|---|---|---|
| page_pan | 0.31 / 0.34 / 0.50 (1.16), over 0 | — | — | 0.11 / 0.16 / 0.76 (1.07) | 0.11 / 0.12 / 0.66 (2.76) |
| page_fling | 0.33 / 0.44 / 0.65 (1.41), over 0 | — | — | 0.12 / 0.75 / 0.92 (1.05) | 0.12 / 0.87 / 1.32 (1.38) |
| deep (×16) | 0.16 / 0.33 / 0.41 (0.55), over 0 | — | — | 0.09 / 0.10 / 0.75 (0.76) | 0.09 / 0.10 / 0.51 (2.62) |
| overview | **8.11 / 8.24 / 8.37**, over **300** | 3.36 / 3.40 / 3.47, over 0 | 1.17 / 1.18 / 1.20, over 0 | 0.17 / 0.20 / 1.72 (2.57); **934 missing tile-frames** | 0.16 / 0.18 / 0.27 (8.43), over 1; 186 missing |
| zoom_sweep | 2.78 / 8.80 / 9.85, over **82** | 1.68 / 4.41 / 4.90, over 0 | 0.86 / 1.89 / 2.09, over 0 | 0.21 / 1.48 / 1.89 (2.05); 169 missing | 0.20 / 2.01 / 3.26 (6.86), over 1; 4 missing |

- CPU per frame for A: p50 0.017–0.18 ms, p99 ≤ 1.22 ms (culling + encoding).
- Visible instances: page ≈ 110k; overview 1.90M (bias 0), 0.83M (bias 1), 0.31M (bias 2).
- Scene build (CPU, all LODs) 551 ms; GPU memory with this simple layout: L0 **251 MB**, all
  LODs **902 MB**. Too much for R-PERF-007; see the decision below.
- **Precision:** the same page rendered with all coordinates offset by 1e7 differs from the
  origin render in 5 of 2,304,000 pixels (zoom 1) and 3 pixels (zoom 16): R-CANVAS-001 holds.
- **LOD appearance:** at overview, ink density visibly changes between bias 0, 1 and 2
  (`evidence/lod-compare.png`; PSNR bias 0 vs 2 = 15.6 dB). Switching levels during a zoom will
  "pop" unless crossfaded or hidden behind tiles.
- **vello (C):** page view 1.47 / 2.29 ms p50/p95 (wall, incl. scene encode 0.27 ms and GPU
  wait); overview **135 / 143 ms**. Two orders of magnitude slower than A at overview, even
  with constant width.

## Decision
1. **Ink renderer = A:** instanced SDF capsule segments in our own wgpu pipeline, chunk-relative
   f32 coordinates with f64 chunk origins, chunk culling, decimation LODs chosen with ~1 px
   tolerance (bias 1). Worst measured: zoom sweep p99 4.90 ms GPU. This meets the budget alone,
   but with little margin.
2. **Tile cache (B) on top for zoomed-out views:** when zoom < 0.25 or visible instances
   > 500k, composite cached tiles (steady state ≤ 0.3 ms). Missing tiles are filled within a
   per-frame budget (8–16 tiles) and covered by the coarser cached level, or by A at a higher
   LOD bias, with a short crossfade. At zoom ≥ 0.25, A draws directly (no tile latency, no
   blur).
3. **Live ink is never tiled:** the active stroke is drawn with A every frame on top; committed
   strokes invalidate the tiles they touch.
4. **Compact GPU format** (to be built in Phase 3/4 and benchmarked against this baseline):
   - shared per-stroke point buffer instead of per-segment duplicates
   - u16 fixed-point chunk-local coordinates (1/32 unit within a 2,048-unit chunk)
   - 8-bit pressure-derived radius, colour/tool by index
   - only levels L0 and L≥3 kept (L1/L2 remove too few points: 7.3M / 6.2M vs 7.9M)

   Target ≤ 250 MB GPU memory for the reference note. That is an estimate: ~8 bytes per sample
   for L0 alone ≈ 64 MB; not measured yet.
5. vello is not used for ink. It may be reconsidered for vector UI/shapes if ever needed (not
   needed for egui).

## Consequences / open
- **Translucent tools** (highlighter): overlapping capsules of one stroke blend twice at joins.
  They need a per-stroke coverage pass (render the stroke to an offscreen layer with max blending,
  then composite). Not measured.
- Fragment-heavy cases (very wide strokes at deep zoom) were not stressed. Deep zoom (×16) of
  normal ink is 0.16 ms.
- Real frame time also includes egui (≤ 0.2 ms p99, ADR 0002) and present. Pen latency is
  unaffected because the canvas never blocks the input path (ADR 0001 rules).
- PDF pages and images are separate textured layers (PDF ADR).

## Update 2026-10-13 (M3.2): tile cache deferred

Measured with the implemented renderer (ADR 0011 format, chunk culling, LOD L0/L3–L6 at ~1 px
tolerance) on the full reference note (100k strokes, 8M samples; 109 MB on the GPU), headless
1920×1200 **including readback**, debug test profile [sim]: zoom 1 1.3–2.1 ms, 0.5 1.6 ms,
0.1 2.8 ms, overview 0.037 2.7 ms (budget 5.5 ms). Decision 2's tile cache was meant to close
the overview gap of strategy A (8.1 ms at LOD bias 0); with LOD the gap is closed. Owner decision
(2026-10-13, "Defer tiles"): **decision 2 is deferred**. Tiles are built only if a real scenario
misses the budget (weaker GPU, PDFs/images on top, live measurements in the app). Decisions 1, 3
(live ink never tiled) and 4 (as amended by ADR 0011) stand.

## Update 2026-10-13 (later): deferral withdrawn, tile cache built (M3.3)

The deferral above rested on one frame per zoom. A fuller test (`r_perf_002_settled_frames_
reference_note`: whole note on the GPU, 20 panned frames at each of 7 zooms down to 1 %) showed
p99 8.5 ms with direct drawing, because zoomed-out frames overdraw densely packed ink. Owner
(2026-10-13, "Build tiles now"): decision 2 applies again. Implemented as decided (256 px tiles,
power-of-two levels shown at scale 0.5–1, coarser cached tile as fallback plus an always-filled
overview level, crossfade counted in frames, chunk versions invalidate tiles, live ink never
tiled). Parameters set by measurement on this renderer [sim, debug harness incl. readback]:

| Parameter | ADR | Now | Why |
|---|---|---|---|
| tile below zoom | 0.25 | **1.0** | direct frames at zoom 0.5 took 3–6 ms (341k instances) |
| or visible instances above | 500k | **250k** | same |
| fills per frame | 8–16 | **2**, and ≤ 100k instances | one zoomed-out fill costs ~1.3 ms (dense ink in 256 px) |

Result: 10/10 runs of both R-PERF-002 tests pass (typical frames 1.1–3.5 ms); a new zoom level
fills over more frames behind its coarser fallback. These deviations are reported to the owner.
