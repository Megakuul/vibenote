# simtab — virtual pen tablet over uinput

Graduated from `spikes/input/simtab` in Phase 3 (reviewed: strict lints, no `unsafe`, checked
conversions, region safety checks). New: `simtab fixture` writes ink fixtures (what the app
receives, emulating libinput's 5 %/1 % tip thresholds) for deterministic engine replay.

Creates a pen tablet through `/dev/uinput` and replays stroke files through it. Events take the
real path (kernel → libinput → compositor → `zwp_tablet_v2` → app).

Device: `BUS_VIRTUAL 1d50:5154`, `INPUT_PROP_POINTER` (or `--direct` for a pen-display),
330×210 mm at 100 units/mm, `ABS_PRESSURE` 0..8191, `ABS_TILT_X/Y` ±9000 at 5730 units/rad
(libinput reads non-zero tilt resolution as units/radian), `ABS_DISTANCE` 0..63,
`BTN_TOOL_PEN`, `BTN_TOOL_RUBBER`, `BTN_TOUCH`, `BTN_STYLUS`, `BTN_STYLUS2`.

```sh
nix-shell                                  # from the repo root
cargo build --release -p simtab
./target/release/simtab fixture --pattern loops --strokes 4 --surface 1200x800 -o loops.jsonl
./target/release/simtab hold --secs 30     # just create the device
./target/release/simtab gen --pattern loops --strokes 4 -o loops.jsonl
./target/release/simtab play loops.jsonl --window <app-class> --log emit.jsonl
./target/release/simtab play --pattern scribble --rate 1000 --region 0.2,0.2,0.5,0.5
```

**Safety:** `play` moves the real cursor and presses the pen down wherever it points. It
refuses to start while a screen locker runs (hyprlock, swaylock, gtklock, waylock) and, during
playback, a watcher checks every 100 ms; on a lock, or when the target window moves, resizes or
leaves the visible workspace, the pen is lifted and playback stops. Use `--window <class>`
(maps into that window with a 10 % margin; refuses if the window is not fully on its monitor or
not on the shown workspace) together with `--window-pid <pid>` (refuses if several windows match
otherwise), or `tools/harness/live-pen.sh`, which isolates the app on its own workspace. For
checks without the compositor, use ``spikes/input/debug-events.sh`` (holds an exclusive
`EVIOCGRAB` on the device).

## Stroke file format (v1, JSONL)
Line 1: `{"format":"simtab-strokes","version":1,"note":"..."}`.
Then one sample per line: `{"t_us":u64,"tool":"pen"|"eraser"|null,"x":0..1,"y":0..1,"p":0..1,
"tx":deg,"ty":deg,"d":0..1,"b1":bool,"b2":bool}`. `tool:null` = out of proximity. `p>0` sets
`BTN_TOUCH`. Coordinates are normalised to the mapped region.

Note: libinput derives tip contact from pressure (tip-down at ≥ 5 %, tip-up at ≤ 1 % of the
range), not from `BTN_TOUCH`; start strokes from a hover with `p=0` (generator does this) so
libinput's pressure-offset heuristic sees a zero baseline.

## Permissions
`/dev/uinput` is normally root-only. Preferred: a udev rule granting the desktop user access
instead of running simtab as root.

Generic Linux (`/etc/udev/rules.d/60-uinput-simtab.rules`):
```
KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"
```
then `sudo udevadm control --reload && sudo udevadm trigger /dev/uinput` (or reboot).
`uaccess` gives the logged-in seat user an ACL; alternatively use `GROUP="input", MODE="0660"`.

NixOS: `hardware.uinput.enable = true;` (adds group `uinput`) and add the user to it, or
`services.udev.extraRules` with the rule above. On the owner's machine the user already has an
ACL (`getfacl /dev/uinput` → `user:linus:rw-`), so nothing had to be changed.

Reading the virtual device with `libinput debug-events` needs read access to
`/dev/input/event*` (group `input`, which the owner is in).
