#!/usr/bin/env bash
# Usage: scripts_debug_events.sh <outfile> <simtab play args...>
# Plays strokes through simtab while `libinput debug-events --grab` holds the device,
# so the compositor never sees the events (safe: nothing on the desktop gets clicked).
set -eu
out=$1; shift
./target/release/simtab play --settle-ms 2500 "$@" --log "$out.simtab.jsonl" &
sp=$!
dev=""
for _ in $(seq 50); do
  for e in /sys/class/input/event*; do
    if [[ "$(cat $e/device/name 2>/dev/null)" == "simtab virtual pen" ]]; then dev=/dev/input/$(basename $e); fi
  done
  [[ -n "$dev" ]] && break; sleep 0.05
done
libinput debug-events --grab --device "$dev" --verbose > "$out" 2>&1 &
lp=$!
wait $sp
sleep 0.3
kill $lp 2>/dev/null || true
wait $lp 2>/dev/null || true
