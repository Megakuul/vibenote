#!/usr/bin/env bash
# Live pen test through the real path (simtab → uinput → libinput → Hyprland → tablet-v2 → app).
# Starts vibenote on a dedicated Hyprland workspace with a scratch workspace DB, replays simtab
# strokes into its window, closes the app (clean shutdown), restores the previous workspace.
#
# Usage: tools/harness/live-pen.sh <out-dir> [simtab play args...]
# Run inside nix-shell (the app dlopens the Vulkan loader from it).
# Env: LIVE_NO_PEN=1 skips simtab (startup bench). VIBENOTE_BIN (default $CARGO_TARGET_DIR/debug/vibenote),
# SIMTAB_BIN (default …/release/simtab). LIVE_WORKSPACE_FROM=<db> starts from a copy of that
# workspace instead of an empty one; LIVE_APP_ARGS adds app arguments (e.g. "--note 7");
# LIVE_AFTER_PEN runs a shell command after the pen replay (APP_PID = the app's pid).
# Safety (owner rules): refuses while the session is locked; the app must be the only window on
# the test workspace and that workspace must be active; never pkill by pattern.
set -eu
out=$1; shift
target=${CARGO_TARGET_DIR:-target}
bin=${VIBENOTE_BIN:-$target/debug/vibenote}
simtab=${SIMTAB_BIN:-$target/release/simtab}
ws=vn-live
mkdir -p "$out"
if pgrep -x hyprlock >/dev/null || pgrep -x swaylock >/dev/null; then echo "session is locked; refusing to run"; exit 2; fi
prev=$(hyprctl activeworkspace -j | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')
app=""
cleanup() {
  if [[ -n "$app" ]] && kill -0 "$app" 2>/dev/null; then
    hyprctl dispatch closewindow "pid:$app" >/dev/null || true
    for _ in $(seq 50); do kill -0 "$app" 2>/dev/null || break; sleep 0.1; done
    kill "$app" 2>/dev/null || true
  fi
  hyprctl dispatch workspace "$prev" >/dev/null
}
trap cleanup EXIT
hyprctl dispatch workspace "name:$ws" >/dev/null
rm -rf "$out/ws.sqlite" "$out/ws.sqlite-wal" "$out/ws.sqlite-shm" "$out/ws.sqlite.blobs"
if [[ -n "${LIVE_WORKSPACE_FROM:-}" ]]; then
  cp "$LIVE_WORKSPACE_FROM" "$out/ws.sqlite"
  # The blob store (PDFs, images, audio) lives next to the database.
  if [[ -d "$LIVE_WORKSPACE_FROM.blobs" ]]; then cp -r "$LIVE_WORKSPACE_FROM.blobs" "$out/ws.sqlite.blobs"; fi
fi
read -r -a app_args <<< "${LIVE_APP_ARGS:-}"
# CLOCK_MONOTONIC right before launch, for the startup bench (python's monotonic_ns = CLOCK_MONOTONIC).
VIBENOTE_LAUNCH_NS=$(python3 -c 'import time;print(time.monotonic_ns())') \
  "$bin" --workspace "$out/ws.sqlite" "${app_args[@]}" > "$out/app.stderr" 2>&1 &
app=$!
clients() { hyprctl clients -j | python3 -c "
import json,sys
c=[c for c in json.load(sys.stdin) if c['workspace']['name']=='$ws']
print(len(c), sum(1 for x in c if x['pid']==$app and x['mapped']))"; }
ok=""
for _ in $(seq 100); do
  read -r n mine < <(clients)
  if [[ "$mine" == 1 ]]; then ok=1; break; fi
  sleep 0.1
done
if [[ -z "$ok" ]]; then echo "vibenote window did not appear"; exit 1; fi
read -r n mine < <(clients)
act=$(hyprctl activeworkspace -j | python3 -c 'import json,sys;print(json.load(sys.stdin)["name"])')
if [[ "$n" != 1 || "$act" != "$ws" ]]; then echo "unsafe workspace state ($n windows, active=$act)"; exit 1; fi
sleep 0.5
hyprctl clients -j | python3 -c "import json,sys;[print(json.dumps({'at':c['at'],'size':c['size'],'monitor':c['monitor']})) for c in json.load(sys.stdin) if c['pid']==$app]" > "$out/window.json"
hyprctl monitors -j > "$out/monitors.json"
if [[ -z "${LIVE_NO_PEN:-}" ]]; then
  "$simtab" play --window vibenote --window-pid "$app" --log "$out/sim.jsonl" --settle-ms 1500 "$@"
fi
# Optional follow-up (e.g. keys for a text box via `hyprctl dispatch sendshortcut ..., pid:$APP_PID`,
# which reaches only the app's window).
if [[ -n "${LIVE_AFTER_PEN:-}" ]]; then
  if pgrep -x hyprlock >/dev/null || pgrep -x swaylock >/dev/null; then echo "session locked; skipping LIVE_AFTER_PEN"; else
    APP_PID=$app bash -c "$LIVE_AFTER_PEN"
  fi
fi
sleep 0.5
# Screenshot of the window (needs grim) for a visual check.
if command -v grim >/dev/null; then
  geo=$(hyprctl clients -j | python3 -c "import json,sys;[print('%d,%d %dx%d'%(c['at'][0],c['at'][1],c['size'][0],c['size'][1])) for c in json.load(sys.stdin) if c['pid']==$app]")
  grim -g "$geo" "$out/window.png" || true
fi
hyprctl dispatch closewindow "pid:$app" >/dev/null
for _ in $(seq 100); do kill -0 "$app" 2>/dev/null || break; sleep 0.1; done
if kill -0 "$app" 2>/dev/null; then echo "app did not exit on close"; exit 1; fi
st=0; wait "$app" || st=$?; echo "app exit: $st"
app=""
if [[ "$st" != 0 ]]; then exit 1; fi
