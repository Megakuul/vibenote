# ADR 0006 — Offline handwriting recognition (de/en)

- **Status:** Accepted (locked) — Gate B, 2026-10-12, owner: "Approve, keep targets". SPEC CER targets stay as quality tests that start red.
- **Date:** 2026-10-12
- **Evidence:** `spikes/hwr/` (`prep.py`, `evaluate.py`, `results.jsonl`, `tractcheck/`).
  Data and models were kept in RAM (`/dev/shm/vibenote-hwr`) because the owner's disk is
  nearly full. Licences in `ATTRIBUTION.md`. CPU timings on the reference machine (4 threads,
  single line per call). Owner decision 2026-10-11: IAM-family data and IAM-trained weights
  are **evaluation-only**; shipped models must be permissive.

## Requirement
R-HWR-003 (offline de/en recognition for search; CER ≤ 10 % en / ≤ 12 % de), R-HWR-007
(pluggable language packs), R-HWR-008 (background, cancellable), R-PERF-010 (block searchable
≤ 3 s after it is finished).

## Survey (sources checked)
- **No permissively licensed stroke-based (online) recogniser or large permissive online
  dataset was found.**
  - BRUSH: non-commercial (Brown CS copyright notice).
  - IAM-OnDB: IAM terms, research only.
  - InkSight (Apache-2.0) converts images to ink and can recognise text, but needs a
    TensorFlow runtime. It was surveyed, not run.
- **Image-based recognisers** therefore read the ink rendered to line images (as the app would
  do).
  - Permissive: PP-OCRv5/v6 (Apache-2.0, ONNX via RapidOCR 3.10.0), GOT-OCR 2.0 (Apache-2.0).
  - Evaluation-only: TrOCR small/base handwritten (MIT weights but IAM-trained), fhswf German
    TrOCR (AFL-3.0, built on IAM-trained TrOCR-large).
  - Not run because they need `trust_remote_code` (executes downloaded code): Florence-2,
    InkSight remote pipelines.

## Eval sets (line level)
| Set | What | Licence | n used |
|---|---|---|---|
| **brush** | BRUSH real pen strokes **rendered by us** like the app's ink (96 px high, 3 px pen) — closest to the product input | eval-only | 200 |
| **iam** | IAM-line test split (scanned pen on paper, English) | eval-only | 200 |
| **de** | fhswf german_handwriting lines (scans, 15 writers, German) | AFL-3.0 | 200 (100 for slow models) |

There is no German **stroke** set. `de` tests German language/charset on scans.

## Results
CER / WER in %, latency per line in ms (p50 / p95), CPU, 4 threads:

| Model | Licence of weights | brush CER / WER | iam CER / WER | de CER / WER | Latency p50 / p95 | Size |
|---|---|---|---|---|---|---|
| **PP-OCRv6 rec small** (multilingual) | Apache-2.0 | **15.5 / 62.0** | **14.6 / 48.7** | **20.7 / 61.9** | 6–14 / 9–38 | 21 MB |
| PP-OCRv6 rec medium | Apache-2.0 | 13.0 / 51.5 | 20.9 / 50.6 | 27.5 / 60.0 | 30–63 / 43–165 | 77 MB |
| PP-OCRv5 mobile EN | Apache-2.0 | 17.1 / 61.8 | 21.3 / 53.0 | 30.2 / 71.3 | 5–9 / 7–22 | 8 MB |
| PP-OCRv5 mobile LATIN | Apache-2.0 | 20.3 / 69.9 | 19.9 / 60.5 | 26.0 / 73.7 | 5–9 / 8–23 | 8 MB |
| GOT-OCR 2.0 | Apache-2.0 | — | — | 58.7 / 105.3 (n=100) | 8,284 / 13,073 | 561 M params |
| TrOCR small (eval-only) | MIT, IAM-trained | 34.1 / 119.7 | 6.0 / 13.8 | 48.7 / 114.6 | 190–264 | 62 M params |
| TrOCR base (eval-only) | MIT, IAM-trained | 24.2 / 101.5 | **4.2 / 9.9** | 32.3 / 96.6 | 1,178–2,028 | 334 M params |
| fhswf German TrOCR (eval-only) | AFL-3.0, IAM-based | 50.1 / 109.2 | 40.6 / 85.1 | **5.6 / 8.4** (n=100; its own training data, so optimistic) | 2,482–4,024 | 558 M params |

Observations:
- TrOCR models are excellent **in their training domain** (IAM 4.2 %, fhswf 5.6 %) but
  transfer poorly to rendered pen ink (24–50 %). They hallucinate long outputs (WER > 100 %) and
  are 100–600× slower. They also cannot ship (owner decision).
- GOT-OCR 2.0 is far too slow and inaccurate on handwriting here. Its remaining runs were
  stopped after the German result.
- **PP-OCRv6 small** is the best shippable model overall: best on iam and de, second on brush,
  6–14 ms per line.
- **The SPEC targets are not met by any shippable model** (best: en 14.6–15.5 % vs. ≤ 10 %;
  de 20.7 % vs. ≤ 12 %).

**Runtime check:** PP-OCRv5 latin mobile and PP-OCRv6 small both run in **tract 0.23** (pure
Rust) with outputs equal to onnxruntime 1.31: max abs difference 5.96e-7 and 4.17e-6 on a real
line. Requires `with_ignore_value_info(true)` / `with_ignore_output_shapes(true)` because of
symbolic dims. tract is 5–7× slower than onnxruntime (PP-OCRv6 small, 333 px line:
32.6 vs 5.9 ms; PP-OCRv5: 58 vs 8.7 ms, the latter measured under CPU contention), still far
inside the 3 s background budget. tract re-optimises per input shape (91–128 ms), so line
widths should be padded to a few buckets with the plans cached.

## Decision
1. **Recogniser: PP-OCRv6 rec small** (Apache-2.0), shipped as a language pack: ONNX model plus
   its character dictionary plus metadata (R-HWR-007). The pack format is ours; a German/English
   pack is the first one.
2. **Pipeline:** finished writing block (ADR 0007) → split into lines → render strokes to a
   48 px-high greyscale line image, exactly as in the eval → CTC recogniser → text with
   per-character confidences → FTS5 index (ADR 0005). Background worker with cancellation; the
   result stores the model/pack version so blocks can be re-recognised after an upgrade.
3. **Runtime: tract** (pure Rust, MIT/Apache): no C++ runtime in-process. onnxruntime via `ort`
   stays the documented fallback if a future model needs operators tract lacks; that would be
   a change proposal.
4. **Search must tolerate recognition errors:** trigram/fuzzy matching on recognised text plus
   the umlaut/ß normalisation from ADR 0005. At ~15–20 % CER, exact-word search alone would
   miss many hits.
5. **Closing the accuracy gap is planned implementation work** (Phase 4 milestones), in this
   order:
   - (a) CTC beam search with a de/en lexicon or small language model. The word-list licence
     must be checked; CC BY-SA lists need owner approval.
   - (b) Fine-tune PP-OCRv6 small on permissive data: fhswf lines (AFL-3.0), rendered strokes
     from permissive handwriting fonts with pen-dynamics jitter, and permissive German/English
     text. Training tooling stays outside the product.
   - (c) Optionally, the owner's own handwriting later (not required).

   The SPEC targets stay as the quality-suite thresholds. Those tests start **red** and drive
   (a)/(b).

## Owner decision (2026-10-12)
- **Keep SPEC targets** — chosen. Options were: keep SPEC targets (en ≤ 10 %, de ≤ 12 % CER) as red tests that implementation must reach,
  **or** adjust them. The proposal is to keep them; the quality suite reports the current
  15.5 % / 20.7 %.
