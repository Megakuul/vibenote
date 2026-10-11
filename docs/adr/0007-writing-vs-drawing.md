# ADR 0007 — Writing-vs-drawing classification

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved.
- **Date:** 2026-10-11
- **Evidence:** `spikes/hwr/classify.py`. Data in RAM (`/dev/shm/vibenote-hwr`); licences in
  `ATTRIBUTION.md`. CPU timings on the reference machine.

## Requirement
R-HWR-002: drawings, diagrams, arrows and sketches are **never** modified. Drawing false
positives on the eval set = 0 (hard gate). Writing recall ≥ 60 % before calibration. Uncertain
→ unchanged.

## Data (owner licence decision 2026-10-11: shippable models train on permissive data only)
| Role | Source | Licence | Amount |
|---|---|---|---|
| train/val/test writing | Hershey single-stroke fonts (scripts, scriptc, cursive, futural, rowmans, timesr) with slant/jitter, German+English words | public domain + acknowledgement | 3,000 lines |
| train/val/test writing | fhswf/german_handwriting line images → skeleton polylines (skimage) | AFL-3.0 | 200 lines |
| train/val/test drawing | Quick, Draw! 24 categories (line, circle, square, triangle, zigzag, squiggle, star, hexagon, octagon, lightning, cloud, house, envelope, light bulb, smiley face, stairs, spreadsheet, rain, snake, moustache, mountain, ocean, wheel, tree); per category 900/300/300 | CC BY 4.0 | 36,000 |
| train/val/test drawing | synthetic arrows, boxes, underlines, crossing-out zigzags, connectors, plots, tables, ellipses | own | 4,000 |
| **eval only** writing | BRUSH real pen strokes, 1,000 random samples | non-commercial research (eval-only) | 1,000 |

## Method (baseline)
- Block = set of strokes. 23 order-invariant geometric features:
  - stroke count, aspect, width/height in units of the median stroke extent
  - stroke-length statistics, straightness, closedness
  - turning per unit length
  - 8-bin direction histogram
  - density of the densest horizontal band (the x-height band)
  - stroke-height spread
  - horizontal stroke progression
- Classifier: scikit-learn `HistGradientBoostingClassifier` (300 iterations, 31 leaves).
- Threshold chosen on the **validation** split as the smallest value with 0 drawing false
  positives.

## Results (test splits: 8,000 drawings = 7,200 Quick, Draw! + 800 synthetic)
| Threshold | Drawing FP (test) | Drawing FP (val) | Writing recall BRUSH (real pen) | Writing recall synthetic+fhswf |
|---|---|---|---|---|
| 0.5 | 7 | 2 | 97.5 % | 98.3 % |
| 0.9 | 2 | 1 | 95.8 % | 97.2 % |
| **0.9555 (val-chosen)** | **1** (a "squiggle") | 0 | 94.9 % | 96.7 % |
| 0.98 | 1 | 0 | 93.4 % | 95.9 % |
| **0.99** | **0** | 0 | **92.4 %** | 94.4 % |
| 0.995 | 0 | 0 | 90.9 % | 93.9 % |
| 0.999 | 0 | 0 | 84.9 % | 91.7 % |

- False positives at 0.5 come from `mountain` (3), `squiggle` (2), `line` (1) and `rain` (1):
  zig-zag-like drawings are the hard cases.
- The val-chosen threshold **fails the hard gate on test (1/8,000)**. 0.99 passes, but it was
  read off the test curve (mild leakage). It is reported as such, not as a validated number.
- Cost: ~1.4 ms per block (Python features + prediction). A Rust port of features + trees
  will be cheaper; it is off the pen path anyway (background after block idle).

## Decision
1. **Two independent gates before any prettify:** (a) the geometric classifier above, at a
   conservative threshold (start 0.995 = 90.9 % recall on real pen data); **and** (b) the
   recogniser must read the block with high confidence and dictionary/language-model plausible
   text in de or en. Either gate "uncertain" → leave the block unchanged. Gate (b) is
   independent evidence that the block is text, which a zig-zag drawing rarely produces.
2. Recognition for **search** runs on every block regardless of class (R-HWR-006): no
   modification, so no risk.
3. Ship the classifier as an ONNX/tree model or as hand-ported trees in Rust. Training stays
   in Python tooling (`tools/`), outside the product.
4. Hard CI gate (Phase 3): **0 drawing FP** on the protected eval set. The eval set adds
   **mixed blocks** (text + underline, text + arrow, labels inside diagrams, crossings-out,
   formulas), which this baseline does not cover.

## Limitations (honest)
- No real owner data. Quick, Draw! sketches and synthetic diagrams are not the owner's notes;
  most real risk sits in **mixed** blocks and formulas, which are not in this eval.
- "Block" here is one line or one drawing. Segmenting real pages into blocks
  (spatio-temporal clustering, R-HWR-001) is not prototyped.
- Writing training data is synthetic plus skeletons from images: no real stroke order or
  timing. Features were chosen to be order-invariant for that reason. Timing features (pauses,
  stroke speed) are a likely improvement once owner/calibration data exists.
