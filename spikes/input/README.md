# Phase 0 input spike (throwaway)

Results: `docs/spikes/input-results.md`. Decision: `docs/adr/0001-input-and-render-path.md`.

```sh
nix-shell spikes/input/shell.nix          # from the repo root; then cd spikes/input
cargo build --release
```

## Try the variants (Gate 0)
Each opens one window; draw with the mouse (fixed pressure) or a pen. Close with the
compositor's close key.

```sh
./target/release/var-gtk                                        # (a) GTK4
INK_SCHED=immediate INK_PRESENT=mailbox ./target/release/var-winit   # (b) winit, best config
INK_PRESENT=mailbox ./target/release/var-wl                     # (c) direct Wayland, GPU
INK_RENDER=shm ./target/release/var-wl                          # (c) direct Wayland, shm partial redraw
INK_PRESENT=mailbox INK_PREDICT_US=8000 ./target/release/var-wl # (c) with 8 ms ink prediction
INK_PRESENT=fifo INK_SCHED=frame_cb ./target/release/var-wl     # (c) "naive" config, for comparison
```

To drive one with the virtual pen while it is open (pen goes only into that window):
```sh
./target/release/simtab play --window inkspike-wl --pattern loops --strokes 4   # or inkspike-winit / dev.inkwell.spike.gtk
```

## Measure
`./measure.sh [filter]` (matrix, ~5 min, needs an unlocked session, uses workspace "inkspike"),
`./mapping.sh` (moved window + scale 1/1.25/1.5; restores scale 1), `./shots.sh`,
`./target/release/analyze results/<run>`, `python3 summarize.py results/*.summary.json`.
Raw logs of the recorded runs: `results/raw-logs.tar.xz`.

Layout: `simtab/` virtual tablet (see its README), `common/` logging + ink mesh + wgpu renderer,
`var-gtk/`, `var-winit/`, `var-wl/` (variants), `analyze/` (latency/axis analysis),
`evidence/` (libinput/udev/wayland-info captures).
