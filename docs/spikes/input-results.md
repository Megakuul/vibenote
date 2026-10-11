# Phase 0 — input & latency spike: results

All numbers are **[sim]**: measured on the owner's machine (`docs/environment.md`) with the
virtual tablet *simtab*. No physical pen existed. **[hw]** re-runs are pending
(`docs/hardware-verification.md`). Date: 2026-10-11.

Code: `spikes/input/` (throwaway). Raw logs: `spikes/input/results/raw-logs.tar.xz`; per-run
summaries: `spikes/input/results/*.summary.json`; libinput/udev evidence: `spikes/input/evidence/`.

## 1. Virtual tablet (simtab)

| Check | Result | Evidence |
|---|---|---|
| udev classification | `ID_INPUT_TABLET=1`, 330×210 mm | `evidence/udev-props.txt` |
| libinput classification | `Capabilities: tablet`, `Size: 330x210mm` | `evidence/libinput-list-devices.txt` |
| Compositor | Hyprland lists it under `Tablets:`; apps get `zwp_tablet_seat_v2.tablet_added` / `tool_added` (`name("simtab virtual pen")`) | `evidence/hyprctl-devices-tablet.txt`, `WAYLAND_DEBUG` trace |
| Pressure | 0..1 reached; tip-down at first sample ≥ 5 % raw (517/8191 → reported 0.05) | `evidence/li-pressure.txt` |
| Tilt | −60°…+60° exact (resolution in units/rad) | `evidence/li-tilt.txt` |
| Eraser end | separate `eraser` tool, own proximity in/out | `evidence/li-eraser.txt` |
| Barrel buttons | `BTN_STYLUS` / `BTN_STYLUS2` press/release | `evidence/li-buttons.txt` |
| Distance / proximity | proximity in/out, distance 1.0→0.13 during approach | `evidence/li-buttons.txt` |
| 1 kHz event rate | all 2000 contact frames → 2001 libinput tip/axis events (1:1); hover-only distance updates after lift partly thinned (79 → 41) | `evidence/li-rate1000.txt` |
| simtab pacing | emit interval median 1000.0 µs, p99 1000.05 µs, max 1015 µs at 1 kHz | `evidence/li-rate1000.txt.simtab.jsonl` |

libinput facts that matter for the product (from libinput source, `src/evdev-tablet.c`):
- Tip contact comes from **pressure thresholds** (default down ≥ 5 %, up ≤ 1 % of the range), not
  from `BTN_TOUCH`; reported pressure is rescaled so the 1 % lower threshold is 0. Very light
  strokes below 5 % are not "down". To check on [hw].
- **Smoothing is disabled for virtual devices** (`use_smoothing = !is_aes && !is_virtual`), so
  real non-AES tablets get extra libinput smoothing latency that simtab does not have.
- Hyprland routes tablet focus via the **pointer focus** surface (`src/managers/input/Tablets.cpp`,
  `refocusTablet`), so the app needs no `wl_pointer` to receive tablet events.

## 2. Variants

| | (a) GTK4 | (b) winit | (c) direct Wayland |
|---|---|---|---|
| Stack | GTK 4.22.4, gtk4-rs 0.11; `EventControllerLegacy` + motion history; custom widget, `GskPath` fills; GSK renderer = `GskVulkanRenderer` | winit **0.31.0-beta.3** + wgpu 30 (Vulkan) | wayland-client 0.31 binding `zwp_tablet_manager_v2` directly; wgpu 30 on the raw `wl_surface`; optional CPU/`wl_shm` path with partial damage |
| Viable? | yes | yes (beta only; 0.30.13 has **no** Wayland tablet code — no tablet/pressure references in `src/platform_impl/linux/wayland`) | yes |
| Presentation timing | `GdkFrameTimings.presentation_time` (GTK gets it from `wp_presentation`, `gdk/wayland/gdkwaylandpresentationtime.c`) | not exposed by winit; spike binds `wp_presentation` on a guest `wayland-client` connection over winit's `wl_display` | own `wp_presentation` (clock_id 1 = CLOCK_MONOTONIC) |

### Axes delivered to the app [sim]

| Axis | (a) GTK | (b) winit 0.31-beta.3 | (c) direct |
|---|---|---|---|
| Pressure | yes, 0..1 (matches libinput rescale, Δ = 0 on contact events) | yes, `Force::Normalized` | yes, 0..65535 |
| Tilt | yes, **normalised −1..1** (GTK declares axis range −90..90, `gdkseat-wayland.c:2915`); ×90 = degrees | yes but **truncated to whole degrees** (`tilt_x as i8`, `winit-wayland src/types/wp_tablet_input_v2.rs`) | yes, degrees (f64) |
| Distance | reported, but values not validated (1.0 / 0.0 seen during approach) | **no** (event ignored) | yes (0.127 during approach, matches libinput) |
| Eraser | yes (`GdkDeviceToolType::Eraser`) — verified | yes (`TabletToolKind::Eraser`) — verified | yes (`type` event) — verified |
| Barrel buttons | yes: BTN_STYLUS → button 2, BTN_STYLUS2 → button 3 — verified | **`BTN_STYLUS` (0x14b) is reported as `Contact`**: pressing it while hovering produced 8 pen-downs for 2 strokes — verified | yes, raw codes 331/332 — verified |
| Proximity | **not observed**: no proximity events reached the widget's `EventControllerLegacy` (not investigated further) | yes (`PointerEntered/Left`) | yes |
| Event timestamp | `gdk_event_get_time` (ms) | **none** (frame `time` dropped) | `frame.time` (ms) |
| Pressure change without motion | delivered | **not delivered** (winit only emits on `Motion`) | delivered |
| Extra events | one extra contact motion with p = 0 at each lift (1505 vs 1500) | same | none |
| Every contact frame delivered | yes (466 of 1505 recovered from GTK motion history; 3978/4500 at 1 kHz) | yes | yes, 1:1 |

**Timestamps:** Wayland `frame.time` − simtab emit time = −0.52 ms median (range −0.63…+1.48)
at 200 Hz and −0.35 ms at 1 kHz, unique per event even at 1 kHz. Hyprland forwards the
kernel/libinput event time truncated to ms. It is usable for [hw] software-latency
measurements; it has no sub-ms resolution. `zwp_input_timestamps_manager_v1` (ns timestamps)
is not offered by Hyprland, and the protocol does not cover tablets anyway.

## 3. Latency method

- simtab reads `CLOCK_MONOTONIC` immediately before `write()`ing each evdev frame and logs it.
- The app logs `CLOCK_MONOTONIC` when it handles each input event (index *i*). For each frame
  *f* it logs the submit time and the last input index drawn. It also logs *f*'s presentation
  time from `wp_presentation` (clock_id 1 = CLOCK_MONOTONIC, verified in the trace).
- Event *i* is "on screen" at the presentation time of the first presented frame with
  last ≥ *i*. **e2e latency = presented − simtab emit.** This is software latency: uinput write →
  kernel → libinput → Hyprland → client → GPU/compositor → scanout (presentation time = vblank
  at which the frame is shown). It excludes the device's own sensing latency and the panel's
  pixel response.
- Breakdown: *in* = app receive − emit; *app>sub* = submit − receive; *sub>pres* = presented −
  submit.
- Matching simtab frames ↔ app events: per stroke, by index when counts agree (1:1 delivery),
  otherwise a bounded nearest-position search. *pos err* = distance between the app's
  surface-local coordinate and the position expected from the tablet→monitor→window mapping.
- Visible-tip lag (prediction): for each presented frame, the drawn tip (incl. predicted
  segment) vs. the simtab position at presentation time. Reported in px and as ms (time when
  the pen was nearest the drawn tip).
- Workload: `scribble`, 5 strokes × 1.5 s, 200 Hz, pressure 0.5, tilt (15°, −10°), unless
  noted. Window tiled 1910×1147 px at scale 1 on the 180 Hz panel (refresh 5.555 ms).

## 4. Results [sim]

All values in ms; e2e = p50 / p95 / p99.

| Run | Config | e2e | in p50/p95 | app>sub p50/p95 | sub>pres p50/p95 |
|---|---|---|---|---|---|
| a1 | **GTK** default (frame clock) | **15.66 / 26.71 / 31.30** | 3.67 / 10.11 | 0.15 / 0.37 | 11.04 / 16.54 |
| a2 | GTK, 1 kHz | 20.30 / 34.47 / 38.05 | 4.44 / 12.92 | 0.41 / 0.93 | 16.01 / 21.80 |
| b1 | winit, `request_redraw` (frame-callback paced), FIFO | 14.32 / 16.83 / 17.07 | 0.04 / 0.41 | 3.37 / 5.55 | 10.96 / 11.14 |
| b2 | winit, draw in event handler, FIFO | 93.61 / 170.02 / 177.41 | 71.09 / 147.76 | 5.56 / 5.62 | 16.97 / 17.00 |
| b3 | **winit**, draw in event handler, Mailbox | **9.21 / 11.58 / 11.85** | 0.03 / 0.09 | 0.08 / 5.08 | 8.42 / 10.89 |
| b4 | winit as b3, 1 kHz | 9.08 / 11.59 / 11.82 | 0.02 / 0.10 | 2.07 / 5.05 | 6.67 / 7.16 |
| c1 | direct, draw on event, FIFO | 30.24 / 33.48 / 33.78 | 3.39 / 9.70 | 5.56 / 11.15 | 17.15 / 22.72 |
| c6 | direct, FIFO, max frame latency 2 | 30.18 / 33.47 / 33.76 | 3.35 / 9.76 | 5.55 / 11.18 | 17.15 / 22.72 |
| c2 | **direct**, draw on event, Mailbox | **9.18 / 11.64 / 11.96** | 0.03 / 0.11 | 0.09 / 5.07 | 8.42 / 10.92 |
| c5 | direct, Vulkan `Immediate` (tearing blocked by compositor) | 9.10 / 11.63 / 11.84 | 0.03 / 0.14 | 0.06 / 5.07 | 8.41 / 10.93 |
| c11 | direct, Immediate + Hyprland `allow_tearing` + `immediate` window rule | 9.13 / 11.64 / 11.91 | — | — | 8.44 / 10.86 |
| c3 | direct, draw on `wl_surface.frame` callback | 18.37 / 22.15 / 22.52 | 0.04 / 0.25 | 4.58 / 10.76 | 11.14 / 16.75 |
| c4 | direct, render at predicted vblank − 2 ms ("late latching"), Mailbox | 10.44 / 12.95 / 13.17 | 0.04 / 0.14 | 2.92 / 5.41 | 7.51 / 7.53 |
| c9 | **direct, CPU `wl_shm` + partial damage**, draw on event | **8.78 / 11.43 / 11.56** | 0.07 / 0.14 | 0.02 / 4.96 | 8.16 / 10.65 |
| c10 | direct, shm partial damage, frame-callback paced | 14.21 / 16.76 / 17.01 | 0.08 / 0.22 | 3.05 / 5.52 | 11.07 / 11.26 |
| c8b / c8a / c8 | direct as c2 at 360 / 500 / 1000 Hz | 9.15 / 9.07 / 9.08 (p95 ≈ 11.6) | ≤ 0.03 / ≤ 0.11 | 2.1–2.3 / 2.9–5.1 | 6.7–7.7 / 7.2–8.9 |

Frames: mailbox runs present at exactly one frame per refresh (interval p50 = p90 = 5.56 ms) and
discard ≈ 10 % of submitted frames, which were replaced before scanout. GTK presents at
5.63 ms p50 but **11.12 ms p90** (missed frames), 6.55 / 16.61 ms at 1 kHz. GTK's
`refresh_interval` reports 16.667 ms although the panel runs at 180 Hz.

### Latency techniques

| Technique | Effect [sim] |
|---|---|
| Draw immediately on input vs. wait for frame callback | −9.2 ms median (c3 18.37 → c2 9.18); winit −5.1 ms (b1 → b3) |
| Non-blocking present (Mailbox) vs. FIFO | FIFO blocks the thread that handles input: c1 30.24 vs c2 9.18; in winit's event handler 93.61 ms (input backs up, b2) |
| Late latching (render at vblank − 2 ms) | slightly **worse** (10.44 vs 9.18); Hyprland latches commits on its own schedule. Only the 2 ms margin was tested |
| Partial redraw (shm + `damage_buffer` of the dirty rect) | −0.3 to −0.4 ms vs. full GPU redraw (c9 8.78 vs c2 9.18; sub>pres 8.16 vs 8.42) |
| Tearing | no effect windowed; Hyprland reports `tearingBlockedBy: … missing candidate` (it tears only for a fullscreen candidate) |
| Ink prediction (linear, 8 ms window) | does not change e2e (real events); **visible tip lag** p50 8.86 → 0.01 ms with 8 ms horizon (px p95 37.4 → 13.8, scribble); 4 ms horizon → 3.94 ms. On loops: 8.89 → 2.38 ms. Overshoot at stroke ends and direction changes not yet judged; needs [hw] feel test |
| Coalescing (drain socket before drawing) | 1 kHz input does not build a backlog (c8 e2e = c2) |

**Floor:** with the best app path, app time is < 0.1 ms p50 and ≈ 5 ms p95 (waiting for a free
buffer). The remainder is compositor + scanout: sub>pres ≈ 8.2–8.4 ms p50 ≈ 1.5 refresh
periods at 180 Hz. The e2e floor on this machine is **≈ 9 ms p50 / 11.5 ms p95 [sim]**. It is
set by Hyprland's repaint/latch timing, not by the client.

### Coordinate mapping [sim]

Floating 900×600 window moved to (137, 211) on the monitor; monitor scale 1, 1.25, 1.5:

| Variant | pos err p95 (logical px), scales 1 / 1.25 / 1.5 | e2e p50 at 1 / 1.25 / 1.5 |
|---|---|---|
| (a) GTK | 0.04 / 0.04 / 0.04 | 14.09 / 13.87 / 13.96 |
| (b) winit | 0.04 / 0.04 / 0.04 | 9.30 / 9.15 / 9.20 |
| (c) direct GPU | 0.04 / 0.04 / 0.04 | 9.20 / 9.10 / 9.12 |
| (c) direct shm | 0.04 / 0.04 / 0.04 | 8.90 / 8.82 / 8.92 |

Screenshots at scale 1.5 (`results/shots-s1.5.png`, plus `results/shot-gtk-fixed.png` after the
GTK winding fix) show the ink rendered at the pen positions in all variants. Only one monitor
exists, so multi-monitor mapping was not testable.

## 5. Caveats

- All latency is **software latency [sim]**. Device sensing latency and libinput smoothing for
  real pens are excluded and must be measured on [hw].
- GTK spike: the live stroke is re-tessellated into one `GskPath` every frame. That is
  O(points) per frame and hurts GTK at 1 kHz (522 frames for 4.5 s of input). Caching
  live-stroke segments would help throughput. It would not remove GTK's structural latency:
  input is dispatched on the frame clock (in = 3.7 ms p50) and presentation is about one frame
  later than mailbox (sub>pres 11.0 vs 8.4 ms).
- An earlier GTK build drew dotted strokes (path winding bug in the spike). It was fixed and
  a1/a2 re-measured; the table shows the fixed numbers.
- winit is a **beta**; its gaps are taken from the 0.31.0-beta.3 source and may change.
- FIFO numbers depend on drawing on the input thread; a separate render thread would avoid the
  blocking, but it would still queue frames.
- Reproduce: `nix-shell spikes/input/shell.nix --run spikes/input/measure.sh` (unlocked
  session; takes over a dedicated workspace for ~5 min), `mapping.sh`, `shots.sh`.
