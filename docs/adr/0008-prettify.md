# ADR 0008 — Prettify (handwriting beautification)

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved.
- **Date:** 2026-10-11
- **Evidence:** `spikes/hwr/prettify.py`; gallery generated in `/dev/shm/vibenote-hwr/gallery/`
  (RAM, ephemeral; regenerate with `python prettify.py`). Eval data: 150 BRUSH lines
  (evaluation-only, owner decision 2026-10-11). No learning involved.

## Requirement
R-HWR-004/005/006: the result stays the owner's handwriting, just more legible. Bounds: glyph
displacement ≤ 0.35·x-height, stroke count/order unchanged, recognised text unchanged; owner
gallery approval. Originals kept, undoable, per-block revert. Mode setting off / suggest /
automatic, default suggest (owner 2026-10-12: no calibration step required).

## Stage 1 prototype (deterministic geometry)
Steps, each partial and clamped:
1. **smooth:** Gaussian along arc length, σ = 0.04·x-height.
2. **rotate:** Theil–Sen baseline through stroke bottoms; rotate if |angle| < 20°.
3. **words:** split on gaps > 0.6·x-height; marks (height < 0.45, width < 0.6 x-height)
   attach to the nearest word.
4. **slant:** per-word slant from near-vertical segments; shear 70 % toward the line median,
   line median reduced 40 %, clamped to ±8°.
5. **size:** x-height 50 % toward the line median, clamped to 0.85–1.18.
6. **base:** word baseline 80 % to the line baseline, clamped ±0.3·x-height.
7. **gap:** word gaps 50 % toward the median, clamped ±0.25·x-height.

Metrics:
- **Legibility proxy:** CER/WER of PP-OCRv5-en on renders before vs. after.
- **Shape displacement:** max point displacement per word after removing that word's
  translation, in x-height units (bound 0.35).
- **Baseline spread:** std of word baselines vs. a robust line, in x-height units.
- Stroke count and point count asserted unchanged (passed on all 150 lines).

## Results (150 lines)
| Steps | CER before → after | WER before → after | Shape disp. p50 / p95 (x-h) | Lines > 0.35 | Baseline spread p50 |
|---|---|---|---|---|---|
| none | 12.72 → 12.72 % | 55.2 → 55.2 % | 0 / 0 | 0 | 0.534 |
| smooth | → 13.13 % | → 56.3 % | 0 / 0 | 0 | 0.532 |
| **rotate** | → 12.68 % | → **51.5 %** | 0 / 0 | 0 | 0.522 |
| slant | → 12.98 % | → 55.5 % | 0.13 / 0.31 | 3 | 0.534 |
| **size** | → **14.46 %** | → 60.3 % | 0.32 / 0.73 | **66** | 0.542 |
| base | → 12.76 % | → 54.2 % | 0 / 0 | 0 | 0.524 |
| gap | → 13.13 % | → 56.8 % | 0 / 0 | 0 | 0.533 |
| rotate+base | → 13.06 % | → 55.0 % | 0 / 0 | 0 | 0.50 |
| rotate+base+slant | → 13.59 % | → 58.2 % | 0.13 / 0.33 | 4 | 0.50 |
| all (incl. size, gap, smooth) | → 14.95 % | → 66.8 % | 0.38 / 0.81 | 81 | 0.51 |

The first unclamped version (before marks were attached to words) was worse: CER 16.7 %, and
quotes/apostrophes were pulled onto the baseline (seen in the gallery).

**Interpretation:**
- Deterministic stage 1 measurably straightens lines (baseline spread 0.534 → 0.50).
- It does **not** improve machine legibility; size normalisation clearly hurts.
- An OCR model is a weak proxy for "looks neater to the owner", so the decisive check is the
  owner's gallery review, on the owner's own handwriting (BRUSH writers write fairly neatly,
  mostly short lines).

## Decision
1. **Stage 1 = rotate + baseline alignment only** (zero shape distortion, bounds trivially met),
   with slant as an opt-in strength slider. Size normalisation is **not** used, except
   possibly later for clear outlier words if the owner chooses to tune it.
2. Prettify is a **display transform over the original strokes**, stored as per-block
   parameters (rotation, per-word offsets, shear): reversible exactly, never destructive
   (R-HWR-005).
3. Mode is a user setting (off / suggest / automatic), default **suggest** (preview, accept per
   block). Always gated by the two classifier gates of ADR 0007. No calibration step is
   required (owner 2026-10-12).
4. Phase 3 harness: `cargo xtask prettify-gallery` produces a before/after HTML gallery for owner
   review; automated bounds become tests.

## Stage 2 (learned, style-preserving re-synthesis) — survey only, no commitment
- **DiffInk** (ICLR 2026, arXiv 2509.23624): latent diffusion transformer generating
  full-line online handwriting from text + reference style (InkVAE with OCR and style losses).
  Code announced as public; licence unknown.
- **Decoupled Style Descriptors** (Kotani et al., ECCV 2020): style-conditioned online
  handwriting generation, trained on BRUSH. **Non-commercial licence** → evaluation-only under
  the owner decision.
- **InkSight** (Google DeepMind, TMLR 2025; Apache-2.0 code and weights for Small-p):
  offline-to-online derendering. Could create permissive stroke training data from permissive
  images. Its model card prohibits handwriting forgery uses.
- Any stage-2 approach needs owner handwriting samples (style reference) and a large
  permissive online training corpus. That does not exist yet; this is the main blocker.

## Patent caution (no legal conclusion)
Handwriting/ink beautification is patented territory, for example:
- US7593574B2 "Ink warping for normalization and beautification" (Microsoft)
- US6771266B2 "Method and apparatus for improving the appearance of digitally represented
  handwriting" (Microsoft)
- US10410316B2 / EP3387582B1 "System and method for beautifying digital ink" (MyScript)
- US20180032494A1 "…beautifying superimposed digital ink" (MyScript)
- US9880990B2 "Leveraging previous instances of handwriting for handwriting beautification"
- US9824266B2 "Handwriting input apparatus…" (style-preserving beautification data)

Some may have expired or may not apply. **The owner should seek legal advice** if vibenote is
ever distributed. For a personal tool this is informational only.
