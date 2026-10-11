#!/usr/bin/env bash
# Mapping checks [sim]: moved floating window, fractional scale 1.25 / 1.5. Restores scale 1.
set -u
cd "$(dirname "$0")"
MON="eDP-1,1920x1200@180,7285x453"
restore() { hyprctl keyword monitor "$MON,1" >/dev/null; }
trap restore EXIT
W="--pattern scribble --strokes 2 --stroke-ms 1000 --rate 200"
move='hyprctl dispatch setfloating class:$class >/dev/null; hyprctl dispatch resizewindowpixel exact 900 600,class:$class >/dev/null; hyprctl dispatch movewindowpixel exact 7422 664,class:$class >/dev/null'
for v in "gtk dev.inkwell.spike.gtk ./target/release/var-gtk" "winit inkspike-winit ./target/release/var-winit" "wl inkspike-wl ./target/release/var-wl" "wlshm inkspike-wl ./target/release/var-wl"; do
  set -- $v; n=$1; class=$2; cmd=$3
  extra=(INK_PRESENT=mailbox INK_SCHED=immediate); [[ $n == wlshm ]] && extra+=(INK_RENDER=shm)
  [[ $n == gtk ]] && extra=()
  for sc in 1 1.25 1.5; do
    hyprctl keyword monitor "$MON,$sc" >/dev/null; sleep 1
    name="m-$n-s$sc"
    env "${extra[@]}" RUN_PREP="$move" ./run.sh "$name" "$class" "$cmd" $W || echo "failed $name"
    ./target/release/analyze "results/$name" > "results/$name.summary.json"
  done
done
