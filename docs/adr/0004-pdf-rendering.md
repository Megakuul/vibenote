# ADR 0004 — PDF rendering and text extraction

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved. Owner: bundle libpdfium.so in a sandboxed worker process.
- **Date:** 2026-10-11
- **Evidence:** `spikes/pdf/` (`pbench`, `fuzz.sh`, typst sources in `docs/*.typ`),
  `spikes/pdf/bench-results.jsonl`, `bench-results-real.txt`, `evidence/`. CPU timings on the
  reference machine (16 threads; single-threaded rendering measured).

## Requirements (SPEC)
R-PDF-001..005, R-PERF-006: a page must be visible at screen resolution ≤ 100 ms after
scrolling into view; import of a 2,000-page PDF ≤ 2 s to the first page; PDF text searchable;
hostile files never crash the app (R-DATA-004); licences permissive or owner-approved.

## Candidates and licences
| Library | Language | Licence | Status |
|---|---|---|---|
| **pdfium** via `pdfium-render` 0.9.4 | C++ (Chromium's PDF engine), dynamically loaded `libpdfium.so` (nixpkgs `pdfium-binaries` 8066) | pdfium BSD-3-Clause (upstream LICENSE); nixpkgs metadata "Apache-2.0 AND BSD-3-Clause AND MIT"; binary packaging (bblanchon/pdfium-binaries) MIT; wrapper MIT OR Apache-2.0 | measured |
| **hayro** 0.8.0 | pure Rust, `#![forbid(unsafe_code)]` | Apache-2.0 OR MIT | measured |
| MuPDF (`mupdf` crate 0.8) | C | **AGPL-3.0** | excluded without owner approval |
| poppler (`poppler-rs` 0.26 binds poppler-glib) | C++ | binding MIT, **library GPL-2.0-or-later** | excluded without owner approval |
| pdf_oxide 0.3.78, lopdf 0.45 | Rust | MIT/Apache, MIT | not renderers (parsing/text); not measured |

## Results

**Generated documents** (typst 0.15.1; `docs/text.pdf` 2,000 pages / 12 MB, `vector.pdf` 200
pages with 600 Bézier curves each, `images.pdf` 50 pages of 2400×3400 noisy greyscale "scans").
Render = one page to RGBA at the given width (1240 px ≈ A4 at fit-width; 2480 = zoom ×2).
30 pages sampled per document. ms:

| Doc | Lib | open | first page | 1240 p50 / p95 / max | 2480 p50 / max | RSS Δ |
|---|---|---|---|---|---|---|
| text (2,000 p) | pdfium | 3.2 | 33.5 | 5.7 / 6.4 / 6.8 | 21.5 / 24.1 | 54 MB |
| text (2,000 p) | hayro | 6.1 | 8.2 | 5.6 / 6.1 / 6.4 | 15.1 / 20.8 | 33 MB |
| vector (200 p) | pdfium | 0.2 | 10.3 | 8.3 / 9.9 / 10.0 | 25.4 / 28.8 | 7 MB |
| vector (200 p) | hayro | 0.7 | 6.5 | 3.6 / 3.9 / 4.1 | 12.2 / 16.3 | 19 MB |
| images (50 p) | pdfium | 0.1 | 56.4 | 51.3 / 55.5 / 55.7 | 93.5 / 96.3 | 75 MB |
| images (50 p) | hayro | 1.3 | 28.0 | 23.0 / 25.5 / 26.2 | 35.7 / 44.3 | 49 MB |

**Real-world documents** (5 arXiv papers, LaTeX/pdfTeX output with figures; downloaded for
local testing only, not stored in the repo). Pages 12–100:

| Paper | pdfium 1240 p50 / p95 / max | hayro 1240 p50 / p95 / max | pdfium 2480 max | hayro 2480 max |
|---|---|---|---|---|
| 1512.03385 | 6.0 / 6.3 / 7.5 | 9.0 / 9.5 / 18.1 | 25.0 | 19.5 |
| 1706.03762 | 5.8 / 22.6 / 33.5 | 6.1 / 47.5 / 56.2 | 77.5 | **190.4** |
| 1810.04805 | 6.8 / 10.8 / 13.4 | 8.6 / 73.3 / **117.1** | 31.5 | **454.7** |
| 2010.11929 | 5.3 / 16.9 / 41.8 | 7.7 / 17.6 / 28.9 | 55.4 | 41.0 |
| 2303.08774 | 5.4 / 8.5 / 27.2 | 6.1 / 8.0 / 25.3 | 40.9 | 33.6 |

**Fidelity:** hayro vs pdfium on the first 12 pages of each paper (60 pages). After a 2 px
Gaussian blur (removes anti-aliasing differences), PSNR min 38.0 dB, p10 39.0, median 40.5.
Visual inspection of the worst page (bar chart) and a diagram page shows no visible content
difference. Unblurred PSNR is 23.6–31.6 dB, the same range as pure glyph anti-aliasing
differences on our generated text page (17.9 dB).

**Encryption** (qpdf 12.3.2): owner-password-only files (AES-256 and RC4-128) render
**pixel-identical** to the plain file in both libraries. With a user password both return a
clean error (`PasswordError` / `Decryption(PasswordProtected)`).

**Text extraction / search:** pdfium `PdfPageText::all()` takes 0.66 ms per page; the whole
2,000-page document takes 4.4 s (7.5 M chars). The search check passes (page 50 contains
"Suchwort-50" and "zebrafisch49"). **hayro offers no text extraction.** `hayro-interpret`
exposes `Glyph::as_unicode()`, which its own docs call "experimental and might not work
reliably" (`hayro-interpret src/font/mod.rs`). A text extractor on top of it was not built.

**Robustness:** 300 mutated files (byte flips, truncations, inserted garbage, from vector.pdf
and images.pdf). Each was opened in a subprocess, rendering 5 pages plus text, with a 10 s
timeout.
- pdfium: 247 ok, 53 clean errors, 0 crashes / panics / hangs
- hayro: 250 ok, 50 clean errors, 0 crashes / panics / hangs

This is a small mutation test, not coverage-guided fuzzing.

## Decision
1. **pdfium is the PDF engine** for rendering and text extraction. It has more consistent
   latency on real documents (max 41.8 ms at fit-width vs hayro's 117 ms, which breaks the
   100 ms budget), is mature (Chromium's engine), and has text extraction for R-PDF-004.
2. pdfium is C++ handling hostile input, so it runs in a **separate worker process**
   (`vibenote-pdfd`), never in the UI/input process. The worker talks over a socket; bitmaps
   come back through shared memory (memfd). A crash or hang of the worker kills only the
   worker: it is restarted, and the page shows an error placeholder. This also keeps pdfium's
   global, non-thread-safe state away from our threads.
3. Pages are rendered **lazily** at the needed resolution, cached as GPU textures (LRU), and
   tiled when zoomed beyond the page bitmap size (shared with the ink tile cache, ADR 0003).
   Text is extracted in the background for indexing, never on the pen path.
4. **hayro** is the documented fallback: pure Rust, faster on our generated documents, but
   with heavier tails on real papers and no text extraction. Revisit when hayro matures; the
   worker-process interface keeps the engine swappable.
5. Imported PDF bytes are stored unmodified (R-PDF-005) in the blob store (storage ADR).

## Owner decisions needed (Gate B)
- **Approve bundling the native `libpdfium.so`** (BSD-3-Clause / Apache-2.0 / MIT; third-party
  notices go into `ATTRIBUTION.md`). The alternative is hayro only, which needs no native
  dependency but risks slow pages and has no PDF text search until we build extraction.
- MuPDF (AGPL-3.0) and poppler (GPL-2.0+) stay excluded unless the owner wants them.

## Open
- Coverage-guided fuzzing of the worker interface (Phase 3 harness).
- PDF features not covered by the generated/arXiv set: forms, annotations, JBIG2/JPX scans,
  very large pages. Add to the fixture corpus in Phase 3 (license-cleared files only).
