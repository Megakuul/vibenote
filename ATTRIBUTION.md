# Attribution and licences

Every bundled asset (fonts, icons, models, sample files) and every dataset or set of model
weights used during development is listed here (AGENTS.md §3). "Use" says whether an item may
ship in vibenote or is **evaluation-only**. Owner decision 2026-10-11: IAM-family data and
IAM-trained weights are evaluation-only; anything shipped must be permissive.

Nothing below is bundled in the product yet. The product's dependency licences will be checked
by `cargo deny` in `xtask ci` (Phase 3).

## Datasets
| Item | Author / source | Licence | Use |
|---|---|---|---|
| Quick, Draw! (simplified ndjson, 24 categories, first 1,500 drawings each) | Google — github.com/googlecreativelab/quickdraw-dataset | CC BY 4.0 | training/eval of writing-vs-drawing (attribution required) |
| german_handwriting (line images) | FH Südwestfalen — huggingface.co/datasets/fhswf/german_handwriting | AFL-3.0 | eval + training (permissive) |
| GNHK | GoodNotes — CC BY 4.0 per its paper; official download page no longer available | CC BY 4.0 | not used (no trustworthy source found) |
| IAM-line (test split) | Teklia re-upload of the IAM Handwriting DB — huggingface.co/datasets/Teklia/IAM-line | labelled MIT, but the underlying IAM terms are non-commercial research | **evaluation-only** |
| BRUSH | Kotani, Tellex, Tompkin (Brown University) — github.com/brownvc/decoupled-style-descriptors | Brown CS copyright notice: non-commercial research only | **evaluation-only** |
| Hershey fonts (via `Hershey-Fonts` 2.1.0, MIT) | Dr. A. V. Hershey, U.S. National Bureau of Standards | public domain with acknowledgement: "The Hershey Fonts were originally created by Dr. A. V. Hershey while working at the U. S. National Bureau of Standards." | synthetic writing (training) |
| Generated test PDFs (`spikes/pdf/docs/*.typ`) | this project, compiled with typst 0.15.1 (Apache-2.0); DejaVu Sans font (Bitstream Vera licence / public domain) | own work | test fixtures |

## Model weights
| Item | Source | Licence | Use |
|---|---|---|---|
| TrOCR small/base handwritten | huggingface.co/microsoft/trocr-*-handwritten | MIT (weights), fine-tuned on IAM | **evaluation-only** (IAM-trained) |
| TrOCR German handwritten | huggingface.co/fhswf/TrOCR_german_handwritten | AFL-3.0, fine-tuned from TrOCR-large-handwritten (IAM-trained) | **evaluation-only** (inherits IAM question) |
| PP-OCRv5 mobile recognition (EN, LATIN), via RapidOCR 3.10.0 | PaddlePaddle / RapidAI | Apache-2.0 | candidate for shipping |
| GOT-OCR 2.0 (hf) | stepfun-ai/GOT-OCR-2.0-hf | Apache-2.0 | candidate (evaluated) |
| InkSight-Small-p | Google DeepMind — huggingface.co/Derendering/InkSight-Small-p | Apache-2.0 (with a stated "no handwriting forgery" usage note) | surveyed, not run |

## Native libraries
| Item | Licence | Use |
|---|---|---|
| pdfium (`libpdfium.so`, nixpkgs pdfium-binaries 8066; bblanchon/pdfium-binaries) | BSD-3-Clause; packaging MIT; nixpkgs: Apache-2.0 AND BSD-3-Clause AND MIT (third-party notices to be collected) | approved by owner 2026-10-11, sandboxed worker process |
| symphonia | MPL-2.0 | approved by owner 2026-10-11 for audio import (unmodified) |

## Fonts (UI)
| Item | Licence | Use |
|---|---|---|
| egui default fonts (`epaint_default_fonts`) | (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 | spike only — **not shipped** (owner 2026-10-11: ship own OFL fonts) |
| Product UI fonts (e.g. Inter, Noto Sans) | OFL-1.1 | to be added with exact versions/sources in Phase 3 |
