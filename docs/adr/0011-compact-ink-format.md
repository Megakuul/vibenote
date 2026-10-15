# ADR 0011 — Compact ink format: lossless per-run offsets instead of the 1/32 grid

- **Status:** Accepted (locked) — owner approved 2026-10-13 ("Approve ADR 0011"); amends ADR 0003 decision 4
- **Context milestone:** M3.1 (GPU-resident chunks). Design note: `plan/design-m3-rendering.md` §0.3, §1.2

## Problem

ADR 0003 decision 4 fixes the GPU point format as "u16 fixed-point chunk-local coordinates (1/32
unit within a 2,048-unit chunk), 8-bit pressure-derived radius". Measured in M3 design (every
protected golden case replayed with its canvas quantised as the format would, compared with the
goldens test's own rule: channel tolerance 6, at most 0.1 % of pixels):

| Quantisation | loops-4 z1 | ramp-3 z1 | ramp-3 z3 | tilt-2 z1 | scribble-5 z1 | scribble-5 z4 | fast-10 z0.5 |
|---|---|---|---|---|---|---|---|
| radius 8-bit only | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 1/32 unit (ADR 0003) + r8 | 0.005 % | 0.0003 % | **0.37 % fail** | 0 | 0.003 % | **0.60 % fail** | 0 |
| 1/64 + r8 | 0 | 0 | 0.009 % | 0 | 0 | **0.12 % fail** | 0 |
| 1/128 + r8 | 0 | 0 | 0 | 0 | 0 | 0.001 % | 0 |

The 1/32 grid fails two protected goldens (zoom 3 and 4). At the maximum zoom (64, R-CANVAS-002)
it is a 2 px grid, against R-CANVAS-001's "no jitter > 0.1 px". The 8-bit radius passes.

## Options

1. **Proposed: lossless per-run offsets, still 8 bytes per sample.** Points store i16 offsets in
   the model's own storage grid (1/1024 unit, `vibenote_model::grid::XY`) from a per-run origin;
   a run (16 B: f32 chunk-local origin x/y, stroke slot, pad) starts whenever a sample is more
   than 32 units from the current origin. Chunk-local coordinates are multiples of 1/1024 below
   16,384 units, so origin + offset is exact in f32: decoding reproduces the stored coordinates
   exactly at every zoom. Everything else in decision 4 stays: shared per-stroke point buffer,
   8-bit radius (relative to the stroke's largest radius), colour/tool by index, levels L0 and
   L≥3. Cost: 2.97 runs per stroke on the reference note (+0.6 B per sample). Estimated GPU
   memory for the reference note: ≈ 86 MB of ink + 128 MB tile atlas ≈ 214 MB (target ≤ 250 MB;
   to be measured in M3.1).
2. **Inside ADR 0003 as written:** keep the 1/32 format resident and add an f32 "fine" cache for
   chunks visible at zoom > 1 (grid error ≤ 1/64 unit passes the zoom ≤ 1 goldens). Two vertex
   formats, a second cache with its own budget/eviction, and a visible switch at zoom 1. More code
   and risk, no benefit. Not recommended.
3. **Finer fixed grid (1/128 or 1/256) with u16:** a 2,048-unit chunk no longer fits in 16 bits
   (needs 18–19 bits); would need smaller chunks (256 units) or 32-bit coordinates (12 B/sample).
   Larger memory or many more chunks; not lossless either.

## Decision (proposed)

Option 1. ADR 0003's decisions 1–3 and 5 are unchanged; decision 4's coordinate format is
replaced by the per-run lossless format above.

## Consequences

- Goldens, R-CANVAS-001 and R-PEN-002 keep passing with no tolerance change (exact coordinates).
- Unit tests: encode/decode round trip is exact (proptest over the model's coordinate range),
  run splitting at the 32-unit bound, hostile/oversized strokes rejected without panics.
- Memory is measured in M3.1 (`FrameStats.gpu_bytes`) against the 250 MB target; if it is
  exceeded, that is reported before continuing.
- Implemented in M3.1 (2026-10-13): measured 90 MB on the GPU for the reference note. The 8-bit
  radius differs from exact radii by at most r_max/510 per point (e.g. 0.04 px for an 11-unit
  stroke at zoom 4), visible only as ≤ 16/255 on anti-aliased edges against the old renderer;
  goldens and R-PEN-002 are unaffected.
