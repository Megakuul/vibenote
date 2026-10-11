# STATUS

- **Current phase:** Phase 2 — Remaining stack spikes (exit: Gate B)
- **Current task:** P2.5 finish ADR 0006 (recognition) → owner approval completes Gate B
- **Last gate passed:** Gate B (partial) — 2026-10-11, owner approved ADRs 0002, 0003, 0004, 0005, 0007, 0008, 0009 ("Approve, 0006 later"). Earlier: Gate A, Gate 0 (2026-10-11).
- **Blockers:** none (owner: no commits needed; work stays uncommitted/staged)
- **Open questions for owner:** ADR 0006 approval (pending benchmarks)

## Gate 0 summary (all [sim])
- e2e software latency p50/p95: (c) direct 9.18/11.64 ms (shm partial redraw 8.78/11.43),
  (b) winit-beta 9.21/11.58 ms, (a) GTK 15.66/26.71 ms. Floor ≈ compositor (≈1.5 refresh at 180 Hz).
- Required rules: draw on input event, never block input on FIFO present, coalesce input,
  wp_presentation for measurement; prediction optional (8 ms horizon removes visible lag p50).
- Hardware checklist open: `docs/hardware-verification.md`.

## Log
- 2026-10-11: Bootstrap started. Created `plan/`, git repo initialised.
- 2026-10-11: P0.1 done (`docs/environment.md`). P0.2 simtab built; udev/libinput/Hyprland classify it as tablet (evidence in `spikes/input/evidence/`).
- 2026-10-11: P0.3 variants (a) GTK4, (b) winit 0.31.0-beta.3 (0.30 has no Wayland tablet support), (c) direct Wayland built.
- 2026-10-11: Session was locked ~20:44 (no measurements possible); resumed after owner unlocked.
- 2026-10-11: P0.4 measurements done (matrix, mapping at scale 1/1.25/1.5, axes, tearing, prediction). P0.5 hardware checklist, P0.7 ADR 0001 written. Incidents during measurement (disclosed to owner): one run moved the spike window off-monitor and simtab pen-taps hit the top-left screen edge (status bar) on the spike workspace; simtab now refuses windows not fully on the monitor. Earlier, two runs tapped the lock screen before the lock-guard existed.
- 2026-10-11: P0.8 staged as "P0: input and latency spike" (commit blocked by signing passphrase).
- 2026-10-11: **Gate 0 passed** (owner approved (c)). Phase 1 started.
- 2026-10-11: Owner answers (Phase 1): PDF pages = vertical column; audio = record + import, ink-synced; undo = per note, persistent; text = basic rich text; links = note/folder, canvas position, web URL (no local files); shapes = tool + snap-from-ink; block idle = 1.5 s; name = "vibenote".
- 2026-10-11: SPEC.md and docs/out-of-scope.md drafted. Waiting for Gate A.
- 2026-10-11: **Gate A passed**. Phase 2 started. Owner: do not commit.
- 2026-10-11: P2.1 UI layer spike done → ADR 0002 (Proposed): egui chrome via egui-wgpu + cosmic-text/glyphon canvas text. Ink latency unchanged under UI load. Gate B owner item: egui default fonts licence (OFL + Ubuntu Font Licence) vs own fonts.
- 2026-10-11: P2.2 rendering at scale → ADR 0003 (Proposed): instanced SDF capsules + chunks + LOD (~1px) + tile cache for zoomed-out; vello rejected for ink (overview 135 ms).
- 2026-10-11: P2.3 PDF → ADR 0004 (Proposed): pdfium in a sandboxed worker process (render + text); hayro documented fallback. MuPDF (AGPL) / poppler (GPL) excluded.
- 2026-10-11: P2.4 storage → ADR 0005 (Proposed): SQLite (WAL, FULL) + R-tree + FTS5 + content-addressed blob store. kill -9: SQLite 200/200, redb 200/200, blobs 100/100. Note: owner disk 98-99% full; spike build intermediates deleted (binaries kept).
- 2026-10-11: Owner decision (HWR licences): IAM-family data and IAM-trained weights = **evaluation only**; shipped models must be permissive.
- 2026-10-11: P2.6 writing-vs-drawing baseline → ADR 0007 (0 FP/8000 at thr 0.99 with 92.4% BRUSH recall; val-chosen thr had 1 FP → two-gate design). P2.7 prettify stage 1 → ADR 0008 (rotate+baseline only; size-normalisation hurts; patent list). P2.8 audio → ADR 0009 (cpal/PipeWire, Opus; loopback 21.4±0.04 ms; ink latency unchanged while recording). ATTRIBUTION.md created.
- 2026-10-11: Owner decisions (Gate B batch): fonts = own OFL fonts (egui defaults disabled); pdfium bundled in sandboxed worker; symphonia (MPL-2.0) allowed for audio import. ADRs 0002–0005, 0007–0009 Accepted (locked).
