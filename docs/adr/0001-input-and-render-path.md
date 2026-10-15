# ADR 0001 — Input and render path

- **Status:** Accepted (locked) — Gate 0, 2026-10-11, owner: "okay take the recommended raw wayland approach"
- **Date:** 2026-10-11
- **Evidence:** `docs/spikes/input-results.md` (all measurements **[sim]**, virtual tablet on the
  owner's machine: Hyprland 0.56.2, AMD 860M/RADV, 180 Hz panel). Real-pen [hw] checks are optional:
  `docs/hardware-verification.md`.

## Context
Pen-to-ink latency is the riskiest requirement ("must feel like paper"). The product targets
Linux/Wayland only, in Rust. Three input/render stacks were built and measured with an identical
workload. The full method is in the results doc §3.

## Options and comparison [sim]

| Criterion | (a) GTK4 | (b) winit 0.31-beta.3 + wgpu | (c) direct Wayland + wgpu |
|---|---|---|---|
| e2e latency p50 / p95 (200 Hz) | 15.66 / 26.71 ms | 9.21 / 11.58 ms (draw in handler, Mailbox) | 9.18 / 11.64 ms (GPU, Mailbox); 8.78 / 11.43 ms (shm partial damage) |
| e2e at 1 kHz p50 / p95 | 20.30 / 34.47 ms | 9.08 / 11.59 ms | 9.08 / 11.58 ms |
| Missed frames (present interval p90) | 11.12 ms (every other frame) | 5.56 ms (none) | 5.56 ms (none) |
| Input delivery | on frame clock, motion-compressed (history recovers points); in = 3.67 ms p50 | immediate (0.03 ms) | immediate (0.03 ms) |
| Pressure / tilt / distance / eraser / buttons | ✓ / ✓ (normalised) / unvalidated / ✓ / ✓ (proximity not observed) | ✓ / integer degrees / ✗ / ✓ / BTN_STYLUS reported as `Contact` (spurious pen-down, verified) | ✓ / ✓ / ✓ / ✓ / ✓ |
| Event timestamps | ms | none | ms (= kernel time, truncated) |
| Pressure-only updates | ✓ | ✗ (only with motion) | ✓ |
| Presentation feedback | via GdkFrameTimings | not exposed (needs a guest connection hack) | native |
| Frame scheduling control | none (GTK frame clock) | partial (redraw requests; present mode) | full (frame callbacks, present mode, own deadline scheduling, fifo/commit-timing available) |
| Partial redraw | GSK internal | no (wgpu: full-surface damage) | yes (shm `damage_buffer`; −0.3–0.4 ms) |
| Mapping (moved window, scale 1/1.25/1.5) | 0.04 px p95 | 0.04 px p95 | 0.04 px p95 |
| UI building blocks | full toolkit | none (window only) | none; window/keyboard/clipboard/IME/cursor must come from libraries (e.g. smithay-client-toolkit) or own code |
| Maturity risk | stable | **beta** API; tablet code new | stable crates; more own code |

Rejected without prototype: **raw evdev reading.** On Wayland a client does not know its
window's position on screen, so it cannot map device coordinates to the surface. Unless it
grabs the device (`EVIOCGRAB`), the compositor sees the same events, so input arrives twice.
It needs read permission on `/dev/input/*` (group `input`, a security cost). It loses
libinput's calibration, pressure thresholds and offset handling, and the compositor's
area/output mapping. Per AGENTS.md it may be prototyped only if (c) shows a specific,
measured problem that evdev would solve. None was found: the input path to the client is
0.03 ms p50, and the floor is the compositor (sub>pres ≈ 8.2–8.4 ms).

## Decision
**(c) Direct Wayland client:** bind `zwp_tablet_manager_v2`, `wp_presentation`,
`wp_fractional_scale_v1` and `wp_viewporter` ourselves, and render the canvas with our own GPU
renderer (wgpu/Vulkan).

Binding rules for the latency path, all backed by the measurements:
1. Process pen events and draw **immediately** on the input event; never wait for a frame
   callback before drawing new ink (−9.2 ms p50).
2. Never block the input thread on presentation. Use a non-blocking present (Vulkan Mailbox),
   or a dedicated render thread that always takes the newest state. FIFO from the input thread
   cost 30–94 ms.
3. Drain all queued input before each draw (coalescing; 1 kHz input causes no backlog).
4. Use `wp_presentation` feedback as the latency instrument in the product and in benchmarks.
5. Ink prediction is a tunable display-only overlay. The default horizon is chosen best effort
   from the [sim] measurements (4–8 ms measured); the owner may tune it later. Predicted ink is
   never stored.
6. Keep partial redraw (damage-limited updates) as an option for the live-stroke layer; it is
   worth ~0.3–0.4 ms here. The main renderer stays GPU (Phase 2 "rendering at scale" decides
   tiling/caching).

Window plumbing (xdg-shell, keyboard/xkb, cursor shape, clipboard, text-input/IME) should come
from established Wayland crates such as smithay-client-toolkit, without giving up the tablet
binding or frame control. Phase 2 spike 1 (UI layer) builds on this and may not reopen it.

Why not (b): it matches (c) on latency today, but it is a beta. It drops distance and
timestamps, truncates tilt, loses pressure-only updates, and reports `BTN_STYLUS` as
`Contact`. Presentation feedback needs a second connection on winit's display. Its only
advantage is portability, which the project explicitly does not value.

Why not (a): +6.5 ms p50 and +15 ms p95 versus (c); it misses frames, and the app cannot
control input dispatch or frame timing. It remains an option for **non-canvas UI only** if
Phase 2 shows it can be combined without touching the canvas path (e.g. separate windows). That
is a Phase 2 question.

## Consequences
- More platform code is ours: window management details, input methods, accessibility. Phase 2
  spike 1 has to cost this.
- `unsafe` is needed at the wgpu raw-surface boundary (`create_surface_unsafe` with the
  `wl_display`/`wl_surface` pointers), and in shm (mmap). It is to be approved in a follow-up
  ADR or encapsulated in a small audited module.
- Latency floor on this machine is ≈ 9 ms p50 / 11.5 ms p95 [sim], dominated by the compositor
  (≈ 1.5 refresh periods). Further gains need compositor-side changes (out of our control) or
  prediction.

## Not covered by [sim] (optional real-pen checks; owner 2026-10-12: not required)
- Device latency, libinput smoothing (active only on real pens), pressure curve and 5 % tip
  threshold, jitter, pen-display vs. screenless mapping, owner feel test (including prediction
  overshoot). If [hw] contradicts this decision, write a change proposal (AGENTS.md §3).
