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
| Quick, Draw! (simplified ndjson, 24 categories, first 1,500 drawings each) | Google — github.com/googlecreativelab/quickdraw-dataset | CC BY 4.0 | training/eval of writing-vs-drawing (attribution required); **2,400 drawings committed in `tests/eval/writing-drawing`** |
| german_handwriting (line images) | FH Südwestfalen — huggingface.co/datasets/fhswf/german_handwriting | AFL-3.0 | eval + training (permissive); **150 lines committed in `tests/eval/recognition/de`**, 40 skeletonised lines in `tests/eval/writing-drawing` |
| GNHK — GoodNotes Handwriting Kollection (test split) | Alex W. C. Lee, Jonathan Chung, Marco Lee (GoodNotes), ICDAR 2021 — github.com/GoodNotes/GNHK-dataset. Official download (goodnotes.com/gnhk) no longer serves the data; obtained 2026-10-13 from the mirror huggingface.co/datasets/staghado/GNHK-Dataset (`GNHK-dataset.zip`, sha256 5f4b4700…be34d, contains the original 2021 `test_data.zip`) | CC BY 4.0 (attribution required; provided "as is"); attribution: GNHK dataset (Lee, Chung, Lee; GoodNotes; ICDAR 2021) | eval (owner approved 2026-10-13); **247 handwritten English lines cropped from the test pages committed in `tests/eval/recognition/en`** (greyscale, 64 px high; lines with printed, illegible or 5+-digit tokens excluded) |
| IAM-line (test split) | Teklia re-upload of the IAM Handwriting DB — huggingface.co/datasets/Teklia/IAM-line | labelled MIT, but the underlying IAM terms are non-commercial research | **evaluation-only** |
| BRUSH | Kotani, Tellex, Tompkin (Brown University) — github.com/brownvc/decoupled-style-descriptors | Brown CS copyright notice: non-commercial research only | **evaluation-only** |
| Hershey fonts (via `Hershey-Fonts` 2.1.0, MIT) | Dr. A. V. Hershey, U.S. National Bureau of Standards | public domain with acknowledgement: "The Hershey Fonts were originally created by Dr. A. V. Hershey while working at the U. S. National Bureau of Standards." | synthetic writing (training) |
| Generated test PDFs (`spikes/pdf/docs/*.typ`) | this project, compiled with typst 0.15.1 (Apache-2.0); DejaVu Sans font (Bitstream Vera licence / public domain) | own work | test fixtures |

## Model weights
| Item | Source | Licence | Use |
|---|---|---|---|
| TrOCR small/base handwritten | huggingface.co/microsoft/trocr-*-handwritten | MIT (weights), fine-tuned on IAM | **evaluation-only** (IAM-trained) |
| TrOCR German handwritten | huggingface.co/fhswf/TrOCR_german_handwritten | AFL-3.0, fine-tuned from TrOCR-large-handwritten (IAM-trained) | **evaluation-only** (inherits IAM question) |
| PP-OCRv6 rec small (multilingual), via RapidOCR 3.10.0 model files (sha256 6f327246…4884) | PaddlePaddle / RapidAI | Apache-2.0 | base of the shipped fine-tuned recogniser (ADR 0006) |
| PP-OCRv5 mobile recognition (EN, LATIN), PP-OCRv6 rec medium | PaddlePaddle / RapidAI | Apache-2.0 | evaluated |
| Fine-tuned recogniser `assets/packs/de-en/rec.onnx` (M9.6; `tools/hwrtrain/finetune.py`, 5 h CPU, best dev checkpoint at step 6,400) | own fine-tuning of PP-OCRv6 rec small (PaddlePaddle / RapidAI, Apache-2.0) on: GNHK **train** split (GoodNotes; Lee, Chung, Lee; ICDAR 2021; CC BY 4.0, attribution: GNHK dataset) and fhswf/german_handwriting (FH Südwestfalen, AFL-3.0; eval/dev lines and every line with the same text excluded) | Apache-2.0 (derived weights) + data attributions as listed | **shipped** (replaces the unmodified PP-OCRv6 rec small; the original is kept at `~/.cache/vibenote/rec-ppocrv6-small-original.onnx`) |
| Writing-vs-drawing classifier `assets/hwr/classifier.json` (gradient-boosted trees, own training with `tools/hwrtrain/train_classifier.py`, scikit-learn 1.9.1) | own model; training data: Quick, Draw! (Google, CC BY 4.0: "Quick, Draw! data made available by Google, Inc. under the Creative Commons Attribution 4.0 International license"), Hershey fonts (public domain, A. V. Hershey, U.S. National Bureau of Standards), fhswf/german_handwriting skeletons (FH Südwestfalen, AFL-3.0), own synthetic diagrams | derived model; attribution as listed | **shipped** (gate A, ADR 0007) |
| Tatoeba sentences, German and English per-language exports (`deu_sentences.tsv.bz2` sha256 7fb0b466…cbfa, `eng_sentences.tsv.bz2` sha256 0eabb52f…5c29, downloaded 2026-10-14 from downloads.tatoeba.org) | Tatoeba contributors — tatoeba.org | CC BY 2.0 FR (attribution: "Sentences from Tatoeba, tatoeba.org, CC BY 2.0 FR") | **shipped** as word frequency ranks in `assets/packs/de-en/lexicon.tsv` (no sentences; `tools/hwrtrain/make_lexicon.py`) |
| SCOWL word lists, sizes ≤ 70 (english/american/british words, upper, contractions, proper names, abbreviations), version 2020.12.07 from nixpkgs `scowl` | Kevin Atkinson and contributors — wordlist.aspell.net | SCOWL notice: "Copyright 2000-2018 by Kevin Atkinson. Permission to use, copy, modify, distribute and sell these word lists, the associated scripts, the output created from the scripts, and its documentation for any purpose is hereby granted without fee, provided that the above copyright notice appears in all copies and that both that copyright notice and this permission notice appear in supporting documentation. Kevin Atkinson makes no representations about the suitability of this array for any purpose. It is provided \"as is\" without express or implied warranty." (parts public domain / ispell notices, see the SCOWL Copyright file) | **shipped** in `assets/packs/de-en/lexicon.tsv` (English words) |
| GOT-OCR 2.0 (hf) | stepfun-ai/GOT-OCR-2.0-hf | Apache-2.0 | evaluated, rejected |
| InkSight-Small-p | Google DeepMind — huggingface.co/Derendering/InkSight-Small-p | Apache-2.0 (with a stated "no handwriting forgery" usage note) | surveyed, not run |

## Native libraries
| Item | Licence | Use |
|---|---|---|
| pdfium (`libpdfium.so`, build 8066 via nixpkgs `pdfium` = bblanchon/pdfium-binaries) — Google / The PDFium Authors (pdfium.googlesource.com); binary packaging Benoît Blanchon | pdfium BSD-3-Clause (with bundled third-party code under Apache-2.0, MIT, BSD-style licences; nixpkgs metadata "Apache-2.0 AND BSD-3-Clause AND MIT"); packaging MIT | **shipped** with the PDF worker `vibenote-pdfd` (ADR 0004, owner approved 2026-10-11); loaded only in the sandboxed worker process. Release packaging must include pdfium's LICENSE and third-party notices. |
| symphonia | MPL-2.0 | approved by owner 2026-10-11 for audio import (unmodified) |

## Fonts (UI)
| Item | Licence | Use |
|---|---|---|
| egui default fonts (`epaint_default_fonts`) | (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 | spike only — **not shipped** (owner 2026-10-11: ship own OFL fonts) |
| Inter 4.1 (`assets/fonts/InterVariable.ttf`, `InterVariable-Italic.ttf`; sha256 4989b125…ccaf, d6f1f6a1…586f) — The Inter Project Authors, github.com/rsms/inter (copied from nixpkgs `inter` 4.1) | OFL-1.1 (`assets/fonts/Inter-LICENSE.txt`, from the v4.1 tag) | **shipped**: canvas text boxes and link labels (render crate, glyphon/cosmic-text; no system fonts) |
| JetBrains Mono NL 2.304 (`assets/fonts/JetBrainsMonoNL-{Regular,Bold,Italic,BoldItalic}.ttf`; sha256 fb3b2575…, 0198e841…, c7392a13…, 6c2716f2…) — The JetBrains Mono Project Authors, github.com/JetBrains/JetBrainsMono (copied from nixpkgs `jetbrains-mono` 2.304; the no-ligature NL cut) | OFL-1.1 (`assets/fonts/JetBrainsMono-LICENSE.txt`: copyright line from the font's name table + the OFL 1.1 text) | **shipped**: text boxes in vim mode (render crate) |
| Lucide icons 1.57.0, subset of 55 icons (`assets/fonts/LucideIcons-subset.ttf`, sha256 d5cff30c…8067; made by `tools/icons/make_subset.sh` from npm `lucide-static` 1.57.0, tarball sha256 afc8602c…a518) — Lucide Icons and Contributors, lucide.dev; icons derived from Feather © Cole Bemis | ISC; Feather-derived icons MIT (`assets/fonts/Lucide-LICENSE.txt`, both notices) | **shipped**: toolbar, tree and canvas-chip icons (ui crate, egui) |

## App icon
| Item | Licence | Use |
|---|---|---|
| `assets/icons/vibenote.svg` — a sheet with ruled lines and a pen stroke; own work (agent, 2026-10-20) | MIT (the project's licence) | **shipped**: desktop entry and AppImage icon (flake package) |
| `assets/icons/hicolor/<size>/vibenote.png` (16–512 px) — rendered from `vibenote.svg` with librsvg's `rsvg-convert` (agent, 2026-10-22) | MIT (the project's licence) | **shipped**: launcher icons (package, AppImage, and the user entry the app installs, R-PLAT-004) |
