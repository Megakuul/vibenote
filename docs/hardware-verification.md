# Hardware verification checklist (optional)

> **Optional, owner's choice, after implementation** (owner, 2026-10-12). No phase, gate,
> milestone, task or test depends on anything in this file. Pen support is implemented best
> effort on the virtual tablet. If the owner later tests with a real pen and something does not
> work, the owner reports or fixes it after implementation.

Phase 0 ran entirely on a virtual tablet (simtab, `spikes/input/simtab`). The items below can be
done whenever a physical pen device is available. Results go into `docs/spikes/input-results.md` with the
label **[hw]** next to the existing **[sim]** numbers. If a [hw] result contradicts a locked
decision, write a change proposal (AGENTS.md §3 "Locked decisions"); do not switch silently.

Owner runs; the agent prepares scripts and analysis.

## A. Device bring-up
- [ ] Record device: model, connection (USB/BT/I²C), pen-display vs. screenless tablet.
- [ ] `libinput list-devices` → capture `Capabilities: tablet`, size, and whether libinput reports
      it as a display tablet. Save to `spikes/input/evidence/hw-list-devices.txt`.
- [ ] `hyprctl devices` lists the tablet.
- [ ] Note libinput behaviour that differs from simtab: libinput applies **smoothing** to real
      non-AES tablets but **not** to virtual (uinput) devices
      (`libinput src/evdev-tablet.c: tablet_init_smoothing → use_smoothing = !is_aes && !is_virtual`).
      Expect slightly higher latency on hw than [sim]; measure it (C).

## B. Axes and ranges
- [ ] `libinput debug-events --verbose`: pressure range 0..1 reached? tilt present and in degrees?
      distance? eraser end detected as separate tool? barrel buttons (BTN_STYLUS, BTN_STYLUS2)?
- [ ] Pressure tip threshold: libinput reports tip-down at ≥ 5 % and tip-up at ≤ 1 % of the raw
      pressure range unless a quirk overrides it (`evdev-tablet.c`, "5 and 1% of the pressure
      range"). Check whether very light strokes are lost; check the device's pressure **offset**
      (worn pens report non-zero idle pressure; libinput auto-detects an offset).
- [ ] Event rate from the device (Hz) and jitter of evdev timestamps.

## C. Latency (re-run all [sim] measurements as [hw])
- [ ] Re-run `spikes/input/measure.sh` with the real pen (manual strokes instead of simtab):
      software latency is still measurable from libinput's event time → presentation time
      (Wayland protocol `frame.time` is in ms, so use the app's receive time; see results doc
      "Method").
- [ ] **Device latency** (pen touches → evdev event): not measurable in software. Options:
      high-speed camera (≥ 240 fps) filming pen tip and screen together; count frames from tip
      motion to ink. Record method and numbers.
- [ ] **Total glass-to-ink latency** with the same camera method, in the chosen variant, for:
      slow stroke, fast stroke, small handwriting.
- [ ] Compare prediction on/off visually (overshoot at stroke ends, hooks at direction changes).

## D. Mapping
- [ ] Pen display: tablet area maps 1:1 onto the panel (`input:tablet:output` in Hyprland if
      needed); check the corners with ruler strokes; check after rotation/scale change.
- [ ] Screenless tablet: aspect ratio preserved? (Hyprland `input:tablet:region_size`,
      `active_area_size`.)
- [ ] Fractional scaling (e.g. 1.25, 1.5) and window moved: ink under the pen tip within ±1 px.

## E. Owner feel test
- [ ] Owner writes a paragraph (German and English), draws a diagram, scribbles fast, writes very
      small, in each viable variant. Owner records verdict: "feels like paper / acceptable / laggy".
- [ ] Owner compares with any reference app they consider good (for feel only, not looks).

## F. Owner handwriting tuning (optional)
- [ ] Record owner fixtures (fixture checklist in AGENTS.md §2 Phase 3 item 4).
- [ ] Label writing vs. drawing; tune classifier thresholds; review prettify gallery.
- [ ] Owner eval set becomes a protected test set.
