# ADR 0002 — UI layer on top of the locked input/render path

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved. Owner: ship own OFL fonts (e.g. Inter + Noto Sans), egui default fonts disabled.
- **Date:** 2026-10-11
- **Builds on:** ADR 0001 (locked): direct Wayland client, own tablet-v2 binding, wgpu renderer.
- **Evidence:** `spikes/ui/` + measurements below (all **[sim]**, owner's machine; method as in
  `docs/spikes/input-results.md` §3). Raw logs: `spikes/input/results/raw-logs-ui.tar.xz`;
  summaries `spikes/input/results/u*.summary.json`; screenshots `u1-ui.png`, `u5-typing.png`.

## Question
How are panels, the folder tree, dialogs and canvas text boxes built without compromising the
pen latency path?

## Options considered
| Option | Compatible with ADR 0001? | Notes / source |
|---|---|---|
| **A. egui (immediate mode) rendered by `egui-wgpu` into our wgpu frame; input fed from our Wayland events** | yes | egui-wgpu 0.36.2 depends on wgpu ^30 (crates.io dependency list); winit integration is an optional feature (`egui-wgpu` Cargo.toml `[features] winit = [...]`) |
| B. iced (`iced_wgpu`) | **no** | iced_wgpu 0.14.0 requires wgpu ^27 (crates.io): a second wgpu in the process, and its shell is winit-based |
| C. GTK (or other toolkit) window hosting our canvas | **no** | the toolkit would own the `wl_surface` and the Wayland event loop, so tablet events and frame timing would go through GTK, which ADR 0001 rejected (GTK measured 15.66/26.71 ms) |
| D. Own retained widgets on wgpu + cosmic-text | yes | full control, but every widget (tree, scroll, dialogs, sliders, focus, a11y) is ours: high cost; no measured advantage over A, because A's cost is already far below the frame budget |
| E. Linebender (vello + parley, masonry/xilem) | partially | vello 0.11 is wgpu ^30 compatible; masonry/xilem are winit-shell based. Vello is evaluated as a renderer in the rendering-at-scale spike, not as UI |

For **canvas text boxes** (rich text at canvas zoom, R-TEXT-001) the candidates were cosmic-text
0.19 (+ glyphon 0.12 for wgpu rendering) and parley 0.12. cosmic-text was prototyped because it
has an `Editor` with per-span attributes (`AttrsList`, `insert_string(text, Option<AttrsList>)`)
and font fallback (`FontSystem`). parley is not ruled out (survey only).

## Measurements [sim]
Spike: our Wayland client from ADR 0001 + egui chrome (toolbar, 300 px folder-tree side panel
virtualised with `ScrollArea::show_rows`, modal settings window) + one cosmic-text/glyphon rich
text box on the canvas + ink. Pen strokes replayed into the canvas area (5 × 1.5 s scribble,
200 Hz), Mailbox, scale 1, 180 Hz.

| Run | Condition | Ink e2e p50 / p95 / p99 (ms) |
|---|---|---|
| baseline (ADR 0001, c2) | no UI | 9.18 / 11.64 / 11.96 |
| u1 | egui chrome, 10k notes; egui re-run only on UI input (cached tessellation otherwise) | 9.43 / 11.88 / 12.13 |
| u2 | egui re-run **every frame**, 100k notes expanded (105,000 rows) | 9.39 / 12.15 / 12.17 |
| u3 | as u2 + tree auto-scrolling 40 px/frame while inking | 9.34 / 12.10 / 12.13 |
| u4 | egui every frame + 2,000-word rich text box on the canvas | 9.40 / 11.64 / 12.19 |

CPU cost per frame (µs, p50 / p99):
| | egui run + tessellate | text prepare (glyphon) | encode + submit |
|---|---|---|---|
| u1 (10k notes) | 91 / 640 | 29 / 111 | 101 / 2,257 |
| u2 (105k rows) | 78 / 162 | 14 / 33 | 80 / 1,350 |
| u3 (scrolling) | 96 / 199 | 14 / 34 | 75 / 1,358 |
| u4 (2,000 words) | 91 / 169 | 82 / 156 | 68 / 1,145 |

- Initial rich-text layout (`set_rich_text` + `shape_until_scroll`) of 2,000 words: 8.0 ms
  (one-off, off the pen path); 60 words: 2.6–3.9 ms. `FontSystem::new()` (system font scan):
  7–9 ms at startup.
- **Typing** into the canvas text box (171 keys via `wtype` virtual keyboard, incl. äöüß ÄÖÜ):
  key event received → presented **9.84 ms p50, 12.37 ms p95, 12.82 ms max**. Umlauts entered
  correctly through xkbcommon with the owner's keymap (screenshot `u5-typing.png`).
- egui text rendering verified (screenshot `u1-ui.png`); egui's default font lacks "▸" (U+25B8),
  shown as a box. The product needs its own font choice anyway.

Interpretation: ink latency stays within noise of the baseline (+0.2 ms p50) under all UI loads.
egui costs < 0.2 ms p99 per frame with 105k virtualised rows, about 3 % of a 5.56 ms frame.

## Decision
1. **App chrome** (toolbars, folder tree, dialogs, settings, search panel): **egui**, rendered
   by `egui-wgpu` as an overlay pass in our frame. Wayland input (pointer, keyboard via
   xkbcommon, pen over chrome) is translated by our code into egui events.
2. **egui runs only when the UI needs it** (UI input arrived, animation or repaint requested).
   Ink-only frames re-draw egui's last tessellation. The pen path never waits on egui layout.
3. **Canvas text boxes** use **cosmic-text** for layout/editing and **glyphon** for GPU text
   rendering, inside our canvas renderer (text boxes are canvas objects, zoomed with the canvas).
   Rich-text formatting is stored in our model; cosmic-text is the layout engine.
4. Large lists (folder tree, search results) are virtualised (`show_rows`-style); nothing scales
   per-item with workspace size per frame.
5. Theme tokens (AGENTS.md §4) map to egui `Style`/`Visuals` and to our canvas colours.

## Consequences / open items
- **IME:** egui has `Event::Ime` (Enabled/Preedit/Commit/…) and outputs `IMEOutput` for the
  cursor rectangle (egui `src/data/input/ime_event.rs`, `src/data/output.rs`). We must bind
  `zwp_text_input_v3` ourselves (offered by Hyprland, `docs/environment.md`). Not prototyped.
- **Accessibility:** egui depends on AccessKit (accesskit 0.24 in egui's Cargo.toml). The Linux
  adapter (`accesskit_unix`, AT-SPI) needs integration with our own event loop. Not prototyped
  and not in SPEC.
- **Clipboard / drag-and-drop:** `wl_data_device` must be bound by us. Not prototyped.
- **Fonts/licences:** egui's bundled default fonts (`epaint_default_fonts`) are under
  "(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0". Bundling them requires
  `ATTRIBUTION.md` rows, and the Ubuntu Font Licence must be accepted. Alternative: ship our own
  OFL fonts (e.g. Inter/Noto) and disable egui's `default_fonts`. **Owner decision at Gate B.**
- All other dependencies of the spike are permissive (MIT/Apache/Zlib/BSD/ISC/Unicode).
  `self_cell` is "Apache-2.0 OR GPL-2.0-only" and is used under Apache-2.0.
- A spike artefact: with egui forced every frame, the spike renders continuously (~10k fps,
  Mailbox), which burns power. The product renders only on change (rule 2).
