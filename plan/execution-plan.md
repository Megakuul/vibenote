# Execution plan

Status: **Draft for Gate D** (2026-10-13). Source of truth for "what next" during implementation.

How to read a task
- **Turns green:** acceptance tests (`tests/acceptance/tests/*.rs`, tracked in `docs/parity.md`)
  and/or crate tests that must pass when the task is done. Every task ends with
  `cargo xtask ci` green and `cargo xtask parity --raise` (the floor only grows).
- **Covers:** SPEC requirement ids. **Crates:** what the task may edit (parallel agents own
  disjoint crates, AGENTS.md §5). **Needs:** task ids that must be done first.
- New unit/integration tests are added freely; protected tests (`tests/golden`,
  `tests/fixtures`, `tests/eval`, `tests/budgets.toml`, `tests/parity_floor.txt`) are never
  weakened. New goldens/fixtures are proposed by the agent and approved by the owner.
- Commands named in tests but not registered yet: `blob.read`, `canvas.hit_test`,
  `hwr.eval_classifier`, `hwr.eval_recognition`, `hwr.packs`, `hwr.segment_blocks`,
  `input.pointer`, `jobs.cancel`, `keymap.get`, `link.status`, `pdf.render_page`,
  `selection.set`, `text.get` — each is registered by the task that needs it.
- App-level reports are read from `<repo>/target/e2e-{latency,startup,app-smoke}.json`
  (repo `target/`, not `$CARGO_TARGET_DIR`); `cargo xtask bench <latency|startup|app-smoke>`
  writes them.

Ordering principle (AGENTS.md Phase 4): latency path and storage first, then scale, then
features by dependency, handwriting after search, UI polish last. Starting point: 13 / 71
requirements pass.

---

## M1 — App shell and the latency path (highest risk first)

Goal: a real Wayland window that inks with the Phase 0 latency, measured end to end.

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M1.1 | Wayland platform in `vibenote-ui`: sctk window (xdg-shell), own `zwp_tablet_v2` binding (port the reviewed spike (c) logic, not copy), `wp_presentation`, fractional scale + viewporter, keyboard/xkb, pointer. Pen events → `input.pen` command. | `ui` unit tests (event translation from recorded protocol logs) | PLAT-002 | ui, apps/vibenote | — |
| M1.2 | Frame loop: wgpu surface with Mailbox, draw live stroke on the input event, drain-before-draw, committed ink via `render` (same pipeline as headless). Prediction overlay (default horizon from Phase 0, setting). | `r_plat_002_wayland_app_runs` via `cargo xtask bench app-smoke` (app starts, simtab stroke → stroke in `state.inspect` over the control socket) | PLAT-002 | ui, render, xtask | M1.1 |
| M1.3 | Control socket in the app (`--control <sock>`), `ui.screenshot`; latency instrumentation (simtab emit timestamp → presentation feedback), `cargo xtask bench latency` writes `target/e2e-latency.json` (≥ 500 strokes, p50/p95). Runs on the owner's session only if unlocked (hyprlock guard). | `r_pen_001_latency_budget_end_to_end`, `r_perf_001_latency` | PEN-001, PERF-001 | ui, control, xtask, tools/simtab | M1.2 |
| M1.4 | Startup path: lazy init (fonts, GPU pipelines cached, workspace metadata only), `cargo xtask bench startup` → `target/e2e-startup.json`. | `r_perf_003_cold_start` | PERF-003 | apps/vibenote, ui, xtask | M1.2, M2.3 |

Risk note: app-level tests need a Wayland session. On the owner's machine they run in the live
session (never while locked). A headless compositor for CI (e.g. a nested/headless Hyprland or
weston) is evaluated in M1.2; if none supports tablet-v2 with uinput, app-level benches stay
`bench`-only and their results are recorded by the agent in STATUS with [sim] labels.

## M2 — Storage off the pen path, persistence completeness

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M2.1 | Storage thread: engine hands pending ops to a channel; group commit ≤ 250 ms; durable-ack sequence numbers; `Engine::sync()` for tests/CLI; errors surface as events, ops retried, never lost. Keep the crash test (25× ci, 1,000× bench) green. | `r_perf_009_save_never_blocks_inking`; keep `r_data_001_*`, `crash.rs` | PERF-009, DATA-001 | storage, engine | — |
| M2.2 | Persistent per-note undo: schema v2 (undo/redo log per scope, bounded 1,000), migration v1→v2 + migration test from a v1 file. | `r_undo_001_persistent_per_note_history` | UNDO-001, DATA-003 | storage, engine, model | M2.1 |
| M2.3 | Lazy open: `Engine::open` loads the tree + note metadata only; `note.open` loads the viewport via R-tree first, streams the rest (background, versioned). Memory stays within budget. | `r_perf_004_open_reference_note`; keep `r_perf_007_memory`, `r_data_002_*` | PERF-004, DATA-002 | storage, engine | M2.1 |
| M2.4 | Settings in the model (tool, eraser mode, prettify mode, block idle, prediction horizon, background), persisted, inspectable at `/settings/...`. | prerequisite for `r_hwr_006_mode`, `r_pen_005_*` | — | model, storage, engine | M2.2 |

## M3 — Rendering at scale (ADR 0003)

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M3.1 | GPU-resident chunks: compact point buffer (lossless per-run i16 offsets on the 1/1024 grid, 8-bit radius; ADR 0011), incremental upload with a per-frame upload budget, chunk culling. | render unit tests; goldens unchanged | PERF-002 | render | — |
| M3.1b | Change feed: `vibenote_model::ChangeLog` per note recorded by the engine (commit, undo/redo, trash empty, lazy-load merges); the UI hands the renderer the changes since its last frame, so the app never walks the whole canvas per frame (design: `plan/design-m3-rendering.md` §2, §5.2–5.4). | render incremental-correctness tests | PERF-002 | model, engine, ui | M3.1 |
| M3.2 | LOD levels (L0, L≥3, ~1 px tolerance) built at commit/load time (off the pen path). | render numeric tests (LOD error ≤ 1 px) | PERF-002 | render, ink | M3.1 |
| M3.3 | Tile cache (deferral withdrawn by the owner after fuller measurements; parameters per ADR 0003 update: tiles below zoom 1 or > 250k instances, 2 fills/frame ≤ 100k instances, coarser fallback + overview level, crossfade). Live stroke never tiled. | `r_perf_002_frame_time_reference_note`, `r_perf_002_settled_frames_reference_note` | PERF-002 | render | M3.2, M2.3 |

Risk note (M3.3): `r_perf_002` times every `render()` call of a fresh renderer, including the
first frame, so uploads and tile fills must be bounded per frame (content completes over
following frames; completeness is checked by goldens, not by this test). The headless
`render()` also reads the image back; if readback alone exceeds the budget, stop and report to
the owner with measurements (protected test; never loosen it).

## M4 — Pen tools and editing

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M4.1 | Tool commands `tool.select.*`, tool state in settings; tilt-responsive highlighter/brush width curves, pen ignores tilt. | `r_pen_005_tool_commands`, `r_pen_003_tilt_affects_tilt_tools_only` | PEN-005, PEN-003 | engine, ink, render | M2.4 |
| M4.2 | Stroke eraser (`ink.erase_stroke`, hit-test against capsule geometry), `canvas.hit_test`. | `r_pen_007_stroke_eraser` | PEN-007 | ink, engine, geom | M4.1 |
| M4.3 | Area eraser: vector split of strokes by eraser footprint, pieces keep samples. | `r_pen_008_area_eraser_splits` | PEN-008 | ink, engine | M4.2 |
| M4.4 | Eraser end in `input.pen` (stroke/area by setting, tool unchanged); barrel buttons: button 1 temporary lasso, button 2 temporary eraser. | `r_pen_006_eraser_end_erases`, `r_pen_010_barrel_button_temporary_lasso` | PEN-006, PEN-010 | engine | M4.3, M4.5 |
| M4.5 | Lasso selection (≥ 50 % inside), `selection.set`, transform (move/scale/rotate), recolour, delete. | `r_pen_009_lasso_and_transform` | PEN-009 | engine, geom, model | M4.2 |
| M4.6 | Mouse input `input.pointer` (draws with pressure 0.5, middle-drag pan). | `r_pen_012_mouse_draws` | PEN-012 | engine | M4.1 |
| M4.7 | View commands: pan, zoom 1 %–6,400 % anchored at the cursor, zoom to fit. | `r_canvas_002_zoom_range_and_anchor` | CANVAS-002 | engine | — |
| M4.8 | Z-order (`object.raise/lower`) and backgrounds (blank/lined/grid/dots; render + goldens proposed to the owner). | `r_canvas_005_z_order`, `r_canvas_006_backgrounds` | CANVAS-005, CANVAS-006 | engine, model, render | M3.1 |
| M4.9 | Clipboard within/between notes (`clipboard.copy/cut/paste`); system clipboard (Wayland data-device) for images/text/PNG in M11. | `r_undo_002_clipboard` | UNDO-002 | engine, model | M4.5 |

## M5 — Organisation and keymap

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M5.1 | Trash: delete → trash, restore, empty (permanent). | `r_org_004_trash` | ORG-004 | model, engine, storage | M2.2 |
| M5.2 | Quick-open: fuzzy name index over 100k notes, ≤ 50 ms per keystroke; recent notes; multi-select move. | `r_org_003_quick_open`, `r_perf_008_quick_open` | ORG-003, PERF-008 | engine, storage | M2.3 |
| M5.3 | Keymap: `keymap.get/set`, SPEC §14 defaults, every binding maps to a registered command. | `r_cmd_002_keymap` | CMD-002 | engine | M4.1 |

## M6 — PDF (ADR 0004)

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M6.1 | Blob store (blake3, durable before referencing txn, GC by reference scan) + `blob.read`. | storage tests (crash during blob write) | DATA-001 | storage, engine | M2.1 |
| M6.2 | `vibenote-pdfd`: pdfium worker, socket protocol, memfd bitmaps, restart on crash/timeout, sandbox (no network, seccomp). Bundled `libpdfium.so` with notices. | pdf crate tests; `r_data_004_hostile_imports` (PDF part) | DATA-004 | pdf, apps/vibenote-pdfd | M6.1 |
| M6.3 | `pdf.import` (one object, vertical column, original bytes kept), `pdf.render_page` lazily at width, page cache; draw over PDFs. | `r_pdf_001_vertical_column`, `r_pdf_005_original_bytes_kept`, `r_perf_006_pdf_page`, `r_canvas_004_draw_over_objects` | PDF-001, PDF-005, PERF-006, CANVAS-004 | pdf, engine, render | M6.2, M4.8 (`object.raise`), M4.2 (`canvas.hit_test`) |
| M6.4 | Detach/rejoin page. | `r_pdf_002_detach_page` | PDF-002 | engine, model | M6.3 |
| M6.5 | Large PDFs: lazy pages, memory bounds, 2,000-page import ≤ 2 s to first page. | `r_pdf_003_lazy_pages_within_budget` | PDF-003 | pdf, render, engine | M6.3 |
| M6.6 | PDF text extraction (background) → search index. | `r_pdf_004_text_searchable` | PDF-004 | pdf, engine, storage | M6.3, M8.1 (index; M6.6 lands before M8.2) |

## M7 — Text boxes, images, links, shapes, ruler

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M7.1 | Text boxes: model (rich runs: bold/italic/underline/strike, 3 headings, lists, checkboxes, colour, size), `text.create/edit/format/get`, cosmic-text layout + glyphon render, own OFL fonts in ATTRIBUTION. | `r_text_001_rich_text`, `r_text_002_umlauts` | TEXT-001, TEXT-002 | model, engine, render | M3.1 |
| M7.2 | Images (PNG/JPEG/WebP/SVG; bounded decode) as objects; all object types create/move/scale/delete/undo/round-trip. | `r_canvas_003_all_object_types` | CANVAS-003 | model, engine, render, storage | M6.3, M7.1, M7.3, M7.4 |
| M7.3 | Links: note/folder, position+zoom, URL (opener stub in tests), `link.status` broken detection, stable ids. | `r_link_001_link_types`, `r_link_002_broken_links`, `r_link_003_stable_ids` | LINK-001..003 | model, engine | M5.1 |
| M7.4 | Shapes: line/arrow/rect/ellipse/polygon objects + `tool.select.shape`. | `r_shape_001_shape_tool` | SHAPE-001 | model, engine, render | M4.1 |
| M7.5 | Snap from ink (hold 500 ms) → line/arrow/rect/ellipse/triangle; undo → original ink; never converts unheld strokes. | `r_shape_002_snap` | SHAPE-002 | ink, engine | M7.4 |
| M7.6 | Ruler: show/hide, move/rotate (15° snap), snap strokes within 12 px of the edge. | `r_ruler_001_snapping` | RULER-001 | engine, ink, render | M4.1 |

## M8 — Search

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M8.1 | Index: our normalisation (casefold, ß→ss, umlaut both forms) + FTS5 + trigram/fuzzy table; sources: note/folder names, text boxes, PDF text, recognised text (version-tagged). | `r_search_003_normalisation` | SEARCH-003 | storage, engine | M7.1 |
| M8.2 | `search.query` (grouped by note, snippets, regions), `search.in_note` (canvas regions). | `r_search_001_all_sources`, `r_search_002_ui_queries`, `r_text_003_searchable`, `r_perf_005_search` | SEARCH-001, SEARCH-002 (engine part), TEXT-003, PERF-005 | engine, storage | M8.1, M6.6 (PDF text) |

## M9 — Handwriting (ADR 0006–0008)

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M9.1 | Background job scheduler (snapshots + versions, cancellation, `jobs.cancel`), never on the pen path. | `r_hwr_008_background` (with M9.4) | HWR-008 | engine | M2.1 |
| M9.2 | Block segmentation (spatial + idle threshold 0.5–10 s, default 1.5 s), `hwr.segment_blocks`, `settings.set_block_idle`. | `r_hwr_001_block_segmentation` | HWR-001 | hwr, engine | M2.4 |
| M9.3 | Writing/drawing gate A: geometric GBDT ported to Rust (training in `tools/`), threshold for 0 drawing FP; `hwr.classify_block`, `hwr.eval_classifier`. | `r_hwr_002_classifier_zero_drawing_fp` | HWR-002 | hwr, engine, tools | M9.2 |
| M9.4 | Recogniser: language pack format + discovery (`hwr.packs`), line rendering at 48 px, PP-OCRv6 small via tract, CTC greedy, results → search index; `hwr.recognize_block`, `hwr.eval_recognition`. Pack de/en (Apache-2.0 weights, ATTRIBUTION). | `r_hwr_007_language_packs`, `r_perf_010_block_searchable`, `r_hwr_008_background` | HWR-007, HWR-008, PERF-010 | hwr, engine, storage | M9.1, M8.1 |
| M9.5 | Accuracy step (a): CTC beam search with de/en lexicon or small LM (licence check; CC BY-SA lists → owner). | progress on `r_hwr_003_recognition_accuracy` (en GNHK baseline 17.3 %, de 18.3 %) | HWR-003 | hwr, tools | M9.4 |
| M9.6 | Accuracy step (b): fine-tune PP-OCRv6 small on permissive data (fhswf train split excluding eval lines, GNHK **train** split, synthetic font renders); tooling in `tools/`, never IAM-family data. | `r_hwr_003_recognition_accuracy` (≤ 10 % en, ≤ 12 % de) | HWR-003 | tools, hwr (pack) | M9.5 |
| M9.7 | Prettify stage 1: display transform (rotation, per-word baseline offsets, optional shear), both gates, mode off/suggest/automatic (default suggest), accept/revert per block; `cargo xtask prettify-gallery`. | `r_hwr_004_prettify_bounds`, `r_hwr_005_revert_identical`, `r_hwr_006_mode` | HWR-004, HWR-005, HWR-006 | hwr, engine, render, xtask | M9.3, M9.4 |

Risk note: M9.6 is research-heavy and may not reach the targets. If it stalls after a
documented effort, stop and report measured numbers to the owner (targets are protected).

## M10 — Audio (ADR 0009)

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M10.1 | Capture/playback (cpal PipeWire), Opus/Ogg chunked encoder → blobs, recording session clock mapping; tests use a null sink / generated source, never the microphone without consent. | `r_audio_001_record_and_import` (record part) | AUDIO-001 | audio, engine, storage | M6.1 |
| M10.2 | Import via symphonia (WAV/FLAC/MP3/Ogg/Opus), bounded decode. | `r_audio_001_record_and_import` | AUDIO-001, DATA-004 | audio | M10.1 |
| M10.3 | Ink ↔ audio sync, `audio.play_at_stroke` (lead-in 2 s); recording never blocks inking. | `r_audio_002_sync`, `r_audio_003_recording_does_not_block_ink` | AUDIO-002, AUDIO-003 | audio, engine | M10.1 |

## M11 — UI chrome and polish (last)

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M11.1 | egui chrome (ADR 0002): toolbar, folder tree (virtualised, drag and drop, colours), settings, own OFL fonts, theme tokens. | UI goldens (proposed to owner); `ui.screenshot` checks | ORG-002/003 UI, CMD-001 | ui | M1.2, M5 |
| M11.2 | Search panel and in-note find UI (Ctrl+Shift+F, Ctrl+F), result highlight. | UI goldens | SEARCH-002 | ui | M8.2 |
| M11.3 | Text editing UI with IME (text-input-v3), system clipboard (data-device), link activation, audio controls, prettify suggest UI. | integration tests via control socket | TEXT-002, UNDO-002, LINK-001, AUDIO-002, HWR-006 | ui | M7, M9.7, M10 |

## M12 — Hardening and harness follow-ups

| Task | What | Turns green | Covers | Crates | Needs |
|---|---|---|---|---|---|
| M12.1 | Fuzzing (cargo-fuzz targets: stroke codec, workspace file, pdfd protocol, image/audio decode, control channel); every crash fix with a regression test. | `r_data_004_*` stays green; fuzz corpus | DATA-004 | all | M6, M7, M10 |
| M12.2 | Budget regression band: apply `[regression] tolerance` from budgets.toml in `xtask bench` (compare to recorded baselines). | xtask tests | PERF-* | xtask | M3, M1.3 |
| M12.3 | `cargo xtask fetch-eval` (optional local IAM/BRUSH comparison, RAM only, eval-only licences printed). | — | HWR-003 | xtask | — |
| M12.4 | Power-loss test (dm-flakey / dm-log-writes) — needs root; **owner decision** before building. | — | DATA-001 | tools | owner |

## Parallelism

After M1.1 and M2.1, these tracks have disjoint crates and can run in parallel agents:
- Track A (ui/render): M1.2–M1.4, M3.
- Track B (storage/engine): M2.2–M2.4, M5, M6.1.
- Track C (hwr, tools): M9.2, M9.3, M9.5 (pure `hwr` work against eval sets).
- Track D (pdf, pdfd): M6.2.
Engine command registration is the usual merge point; merge only when `xtask ci` is green.

## Owner decisions expected during implementation
- New goldens (backgrounds, UI) and any new fixtures: owner approves (`cargo xtask golden --approve`, `cargo xtask protect --write`).
- Lexicon/word-list licences that are not clearly permissive (M9.5).
- Power-loss test with root/device-mapper (M12.4).
- Any change proposal against a locked ADR.
