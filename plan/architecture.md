# vibenote architecture

Derived from the locked ADRs (`docs/adr/0001`–`0009`, all Accepted (locked)) and the Phase 3
skeleton. When this file and an ADR disagree, the ADR wins; fix this file.

## 1. Crates and layering

```
geom ─► model ─┬─► storage ─────────────┐
               ├─► ink ─┬─► hwr ────────┤
               │        └─► render ─────┤
               ├─► pdf ─────────────────┤
               └─► audio ───────────────┴─► engine ─► control
                                                 └──► ui ─► apps/vibenote
apps/vibenote-cli (engine, control, render)   apps/vibenote-pdfd (pdf)   tools/simtab (ink)
```

Enforced by `cargo xtask layers` (allowed edges in `xtask/src/layers.rs`; Wayland/egui/winit and
similar crates are allowed **only** in `vibenote-ui` and `apps/vibenote`).

| Crate | Owns | ADR |
|---|---|---|
| `geom` | Point, Rect, Affine (f64), hit-test helpers | — |
| `model` | Workspace tree, notes, canvases, objects, strokes, `Op` with exact inverses, sample quantisation grid | 0005 |
| `ink` | pen events, fixture format v1, width curves (pressure/tilt per tool), stroke geometry (erase/split, LOD decimation) | 0001, 0003 |
| `storage` | SQLite (WAL, `synchronous=FULL`), schema + migrations, stroke codec S1, R-tree, FTS5, blob store (blake3), storage thread | 0005 |
| `hwr` | block segmentation, writing/drawing classifier, recogniser (tract, language packs), prettify transforms | 0006, 0007, 0008 |
| `pdf` | client side of the pdfd protocol (requests, memfd bitmaps), PDF object layout (vertical column) | 0004 |
| `audio` | cpal/PipeWire capture/playback, Opus/Ogg chunked encoder, symphonia import, clock mapping | 0009 |
| `render` | wgpu (Vulkan) ink renderer: instanced SDF capsules, chunks, LOD, tile cache, PDF/image textures, glyphon text; headless mode | 0002, 0003 |
| `engine` | `Session` (state, view, live input, settings), command registry, undo per scope, background job scheduler, storage/worker handles | — |
| `control` | JSON-lines control channel (stdio / Unix socket), method table | — |
| `ui` | Wayland client (smithay-client-toolkit + own `zwp_tablet_v2` binding, `wp_presentation`, fractional scale), frame loop, egui chrome via egui-wgpu, keymap → command dispatch, theme tokens | 0001, 0002 |
| `apps/vibenote` | `main`: config, logging, starts ui + engine + workers | — |
| `apps/vibenote-cli` | headless CLI: commands, run, script, serve, inspect, replay, render-png | — |
| `apps/vibenote-pdfd` | sandboxed pdfium worker process | 0004 |

## 2. Threads and processes

```
                ┌─────────────── vibenote process ───────────────────────────────┐
 Wayland ──► [input/render thread] ── ops ──► [storage thread] ──► SQLite + blobs│
 events       engine.execute, live ink,          group commit ≤ 250 ms, fsync    │
              draw immediately, mailbox present                                    │
                 │  ▲ results (versioned, cancellable)                             │
                 ▼  │                                                              │
              [job pool] hwr: segment → classify → recognise → index; prettify    │
              [audio RT thread] cpal ─► ring buffer ─► [encoder thread] ─► blobs   │
                 │                                                                 │
                 └── socket + memfd ──► vibenote-pdfd (pdfium, separate process) ──┘
```

Rules (ADR 0001 binding rules, AGENTS.md §4):
1. The input/render thread processes pen events and draws **on the event**; it never waits for a
   frame callback, storage, recognition, PDF rendering, audio or a lock held by background work.
2. Presentation is non-blocking (Vulkan Mailbox). All queued input is drained before a draw.
3. `wp_presentation` feedback is the latency instrument (product telemetry + `xtask bench latency`).
4. Ink prediction is a display-only overlay; predicted samples are never stored.
5. Background work gets immutable snapshots plus a version; results whose version is stale are
   dropped (cancellation, R-HWR-008).
6. Engine behaviour depends only on inputs (commands, fixtures, seeds). Wall-clock time enters
   only through command params (event timestamps), so every bug reproduces from a recording.

## 3. Data model and persistence

- Everything changes through `Op`s; `Workspace::apply(op)` returns the exact inverse;
  `apply_all` is atomic (rollback on error). A command = one undo `Step` in a scope
  (`workspace` or `note:<id>`), bounded to 1,000 steps (R-UNDO-001), **persisted** with the note.
- Samples are quantised on insert (x/y 1/1024 unit, pressure 1/65535, tilt 1/100°), so
  save → load is exact and goldens/replays are bit-stable.
- The engine appends applied ops to a pending journal; the storage thread commits them in
  groups (≤ 250 ms) with WAL + `synchronous=FULL`. Ack-to-durable ≤ 1 s (R-PERF-009 /
  R-DATA-001).
- Large binaries (PDF, images, audio chunks) go to the content-addressed blob store; a blob is
  durable **before** the transaction that references it. Original bytes are never modified.
- Open: tree metadata first; a note opens with an R-tree viewport query and streams the rest.
- Search: our normalisation (casefold, ß→ss, umlaut both forms) → FTS5 + trigram/fuzzy for
  recognised text (ADR 0005/0006).
- Schema versioned with `PRAGMA user_version`; forward migrations in transactions, tested from
  every released version.

## 4. Rendering (ADR 0003, 0002)

- Committed ink: instanced SDF capsules from a shared per-stroke point buffer, chunk-relative
  coordinates (f64 chunk origins, u16 fixed-point inside 2,048-unit chunks), chunk culling,
  LOD with ~1 px tolerance (levels L0 and L≥3).
- Zoom < 0.25 or > 500k visible instances: tile cache composite; missing tiles filled within a
  per-frame budget, covered by coarser tiles.
- Live stroke: always drawn directly on top, never tiled; optional damage-limited redraw.
- PDF pages and images: GPU textures (LRU), tiled when zoomed beyond bitmap size.
- Text boxes: cosmic-text layout + glyphon rendering inside the canvas pass.
- Chrome: egui via egui-wgpu as an overlay pass; egui only runs when the UI needs it; ink-only
  frames reuse the last tessellation. Own OFL fonts (egui default fonts disabled).
- Headless renderer (same pipeline, offscreen) is the source of goldens and screenshots.

## 5. Handwriting pipeline (ADR 0006–0008)

```
pen-up ─► block segmentation (spatial + 1.5 s idle) ─► finished block
   ├─► recogniser (always; render line → 48 px image → PP-OCRv6 small via tract → CTC
   │     → text + confidences) ─► search index (version-tagged; re-run after pack upgrade)
   └─► gate A: geometric classifier (threshold for 0 drawing FP)  ┐ both "writing"
       gate B: recogniser confidence + lexicon plausibility (de/en)┘   ─► prettify candidate
           prettify = display transform (rotation, per-word baseline offsets, optional shear)
           mode off / suggest (default) / automatic; originals untouched; per-block revert
```

Language packs: directory with `manifest.json` (format, version, lang, model, dictionary,
lexicon); discovered at start (R-HWR-007). Training tooling lives in `tools/`, never in the
product.

## 6. PDF (ADR 0004)

`vibenote-pdfd` runs pdfium in a separate process (seccomp/no network, restarted on crash or
timeout). Requests over a Unix socket; bitmaps returned via memfd. A PDF is one canvas object
laid out as a vertical column of pages with a fixed gap; pages render lazily at the needed
resolution; text is extracted in the background for search. Fallback engine: hayro (change
proposal needed).

## 7. Audio (ADR 0009)

cpal (PipeWire) on its own RT thread → lock-free ring buffer → encoder thread (Opus/Ogg, 10 s
chunks, each durably committed) → blob store. Each recording stores CLOCK_MONOTONIC of sample
0; each stroke stores its CLOCK_MONOTONIC start; seek = (stroke − start) − lead-in (2 s).
Import via symphonia (MPL-2.0, unmodified; owner approved).

## 8. Commands and control

Every user-visible action is a registered command (`id`, JSON-schema params, requirement ids,
`enabled`, `run`). UI, keymap, CLI and control channel dispatch by id; programmatic calls never
open dialogs. Control channel methods: `ping`, `engine.commands`, `engine.execute`,
`state.inspect`, `control.methods` (+ `ui.screenshot` once the app exists). Agents verify via
`state.inspect` first, screenshots second.

## 9. Safety

- Workspace lints: no `unsafe`, `unwrap`, `expect`, `panic!`, indexing, unchecked casts or
  arithmetic in product code. Errors are crate `Result`s; unfinished features return
  `NotImplemented`.
- All input is hostile (files, PDFs, images, audio, pen events, control params): bounded
  allocation, recursion and sizes; pdfium only out of process.
- Licences: `cargo deny` in ci; assets in `ATTRIBUTION.md`; IAM-family data/weights are
  evaluation-only and never shipped.
