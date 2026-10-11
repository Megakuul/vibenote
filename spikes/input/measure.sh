#!/usr/bin/env bash
# Phase 0 measurement matrix [sim]. Requires an unlocked Hyprland session; takes over the
# screen (dedicated workspace) for a few minutes. Usage: nix-shell shell.nix --run ./measure.sh [filter]
set -u
cd "$(dirname "$0")"
cargo build --release -q -p simtab -p var-gtk -p var-winit -p var-wl -p analyze || exit 1
filter=${1:-}
W="--pattern scribble --strokes 5 --stroke-ms 1500 --rate 200"
W1K="--pattern scribble --strokes 3 --stroke-ms 1500 --rate 1000"
run() { # name class cmd env... -- simtab args
  local name=$1 class=$2 cmd=$3; shift 3
  [[ -n "$filter" && "$name" != *$filter* ]] && return
  local envs=(); while [[ $# -gt 0 && "$1" != "--" ]]; do envs+=("$1"); shift; done; shift
  echo "== $name"
  env "${envs[@]}" ./run.sh "$name" "$class" "$cmd" "$@" || { echo "run failed: $name"; return; }
  ./target/release/analyze "results/$name" > "results/$name.summary.json"
  python3 -I - "results/$name.summary.json" <<'PY'
import json,sys
s=json.load(open(sys.argv[1])); l=s["latency_ms"]["e2e_sim_to_present"] or {}
print(f'   e2e p50={l.get("p50")} p95={l.get("p95")} p99={l.get("p99")} max={l.get("max")}  events {s["contact_events"]}  frames {s["frames"]["presented"]}/{s["frames"]["drawn"]}')
PY
}
GTK=./target/release/var-gtk; WINIT=./target/release/var-winit; WL=./target/release/var-wl
run a1-gtk            dev.inkwell.spike.gtk "$GTK"   INK_X=1                                   -- $W
run b1-winit-redraw   inkspike-winit "$WINIT" INK_SCHED=redraw INK_PRESENT=fifo                -- $W
run b2-winit-imm-fifo inkspike-winit "$WINIT" INK_SCHED=immediate INK_PRESENT=fifo             -- $W
run b3-winit-imm-mbx  inkspike-winit "$WINIT" INK_SCHED=immediate INK_PRESENT=mailbox          -- $W
run c1-wl-imm-fifo    inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=fifo                   -- $W
run c2-wl-imm-mbx     inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox                -- $W
run c3-wl-framecb     inkspike-wl "$WL" INK_SCHED=frame_cb INK_PRESENT=fifo                    -- $W
run c4-wl-deadline    inkspike-wl "$WL" INK_SCHED=deadline INK_PRESENT=mailbox INK_DEADLINE_US=2000 -- $W
run c5-wl-imm-tear    inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=immediate              -- $W
run c6-wl-fifo-lat2   inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=fifo INK_FRAME_LATENCY=2 -- $W
run c7-wl-mbx-pred    inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox INK_PREDICT_US=8000 -- $W
run c7b-wl-mbx-pred4  inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox INK_PREDICT_US=4000 -- $W
run c7c-wl-mbx-pred-loops inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox INK_PREDICT_US=8000 -- --pattern loops --strokes 5 --stroke-ms 1500 --rate 200
run c7d-wl-mbx-loops  inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox -- --pattern loops --strokes 5 --stroke-ms 1500 --rate 200
run c9-wl-shm-imm     inkspike-wl "$WL" INK_RENDER=shm INK_SCHED=immediate                    -- $W
run c10-wl-shm-fcb    inkspike-wl "$WL" INK_RENDER=shm INK_SCHED=frame_cb                     -- $W
run a2-gtk-1k         dev.inkwell.spike.gtk "$GTK"   INK_X=1                                   -- $W1K
run b4-winit-1k       inkspike-winit "$WINIT" INK_SCHED=immediate INK_PRESENT=mailbox          -- $W1K
run c8a-wl-500        inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox                -- --pattern scribble --strokes 3 --stroke-ms 1500 --rate 500
run c8b-wl-360        inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox                -- --pattern scribble --strokes 3 --stroke-ms 1500 --rate 360
run c8-wl-1k          inkspike-wl "$WL" INK_SCHED=immediate INK_PRESENT=mailbox                -- $W1K
