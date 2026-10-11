#!/usr/bin/env bash
# Run one measurement: start a variant on a dedicated Hyprland workspace, replay strokes
# through simtab into its window, collect logs, restore the previous workspace.
#
# Usage: run.sh <name> <app-class> "<app command>" [simtab play args...]
# Env vars for the app (INK_*) are passed through. Output: results/<name>.{app,sim}.jsonl
set -eu
name=$1; class=$2; cmd=$3; shift 3
mkdir -p results
# Never inject pen input while the session is locked (events would go to the lock screen).
if pgrep -x hyprlock >/dev/null || pgrep -x swaylock >/dev/null; then echo "session is locked; refusing to run"; exit 2; fi
prev=$(hyprctl activeworkspace -j | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')
restore() { hyprctl dispatch workspace "$prev" >/dev/null; }
trap restore EXIT
hyprctl dispatch workspace name:inkspike >/dev/null
INK_LOG="results/$name.app.jsonl" bash -c "exec $cmd" > "results/$name.stderr" 2>&1 &
app=$!
ok=""
for _ in $(seq 100); do
  if hyprctl clients -j | python3 -c "
import json,sys
c=[c for c in json.load(sys.stdin) if c['class']=='$class' and c['workspace']['name']=='name:inkspike' or (c['class']=='$class' and c['workspace']['name']=='inkspike')]
sys.exit(0 if c else 1)"; then ok=1; break; fi
  sleep 0.1
done
if [[ -z "$ok" ]]; then echo "window $class did not appear"; kill $app 2>/dev/null; exit 1; fi
# Safety: the window must be the only one on the workspace and the workspace must be active.
n=$(hyprctl clients -j | python3 -c "import json,sys;print(sum(1 for c in json.load(sys.stdin) if c['workspace']['name'] in ('inkspike','name:inkspike')))")
act=$(hyprctl activeworkspace -j | python3 -c 'import json,sys;print(json.load(sys.stdin)["name"])')
if [[ "$n" != 1 || ( "$act" != inkspike && "$act" != name:inkspike ) ]]; then echo "unsafe workspace state ($n windows, active=$act)"; kill $app; exit 1; fi
if [[ -n "${RUN_PREP:-}" ]]; then eval "$RUN_PREP"; sleep 0.5; fi
sleep 0.5
hyprctl clients -j | python3 -c "import json,sys;[print(json.dumps({'at':c['at'],'size':c['size']})) for c in json.load(sys.stdin) if c['class']=='$class']" > "results/$name.window.json"
hyprctl monitors -j > "results/$name.monitors.json"
./target/release/simtab play --window "$class" --log "results/$name.sim.jsonl" --settle-ms 1500 "$@"
sleep 0.5
if [[ -n "${RUN_POST:-}" ]]; then eval "$RUN_POST"; fi
kill $app 2>/dev/null || true
wait $app 2>/dev/null || true
