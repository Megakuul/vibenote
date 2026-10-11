#!/usr/bin/env bash
# Screenshots of each variant after replaying loops, at monitor scale 1.5 (restores 1).
set -u
cd "$(dirname "$0")"
MON="eDP-1,1920x1200@180,7285x453"
trap 'hyprctl keyword monitor "$MON,1" >/dev/null' EXIT
hyprctl keyword monitor "$MON,1.5" >/dev/null; sleep 1
shot='g=$(hyprctl clients -j | python3 -c "import json,sys;c=[c for c in json.load(sys.stdin) if c[\"class\"]==\"$class\"][0];print(f\"{c[\"at\"][0]},{c[\"at\"][1]} {c[\"size\"][0]}x{c[\"size\"][1]}\")"); grim -g "$g" results/$name.png'
for v in "gtk dev.inkwell.spike.gtk ./target/release/var-gtk" "winit inkspike-winit ./target/release/var-winit" "wl inkspike-wl ./target/release/var-wl" "wlshm inkspike-wl ./target/release/var-wl"; do
  set -- $v; n=$1; class=$2; cmd=$3; name="shot-$n-s1.5"
  extra=(INK_PRESENT=mailbox); [[ $n == wlshm ]] && extra+=(INK_RENDER=shm)
  env "${extra[@]}" RUN_POST="$shot" ./run.sh "$name" "$class" "$cmd" --pattern loops --strokes 4 --stroke-ms 1500 || echo "failed $name"
done
