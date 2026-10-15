#!/usr/bin/env bash
# Harness self-validation: for each deliberate fault, copy the repo to a scratch dir, apply the
# fault (must match exactly once — otherwise the fault is reported as PATCH-FAILED and ci is not
# run), run `cargo xtask ci`, record which steps failed. Never touches the working tree.
# Usage (repo root, inside nix-shell): tools/harness/validate.sh [fault-name...]
set -u
REPO=$(cd "$(dirname "$0")/../.." && pwd)
SCRATCH=${SCRATCH:-/dev/shm/vibenote-faults}
export CARGO_TARGET_DIR=${FAULT_TARGET:-/dev/shm/vibenote-target-faults}
export CARGO_INCREMENTAL=0
OUT=$REPO/docs/harness-validation.raw.txt
names="baseline $(python3 -I -c "import sys; sys.path.insert(0,'$REPO/tools/harness'); from faults import FAULTS; print(' '.join(f[0] for f in FAULTS))")"
[ $# -gt 0 ] && names="$*"
: > "$OUT"
for n in $names; do
  rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"
  # --no-times: every copy gets fresh mtimes. With preserved (old) mtimes cargo would treat a
  # restored file as older than the artefact built from the previous fault and reuse it.
  if ! rsync -a --no-times --exclude target --exclude spikes --exclude .git "$REPO/" "$SCRATCH/"; then
    echo "$n | COPY-FAILED" | tee -a "$OUT"; continue
  fi
  if [ "$n" != baseline ] && ! python3 -I - "$SCRATCH" "$n" <<'PY'
import sys
sys.path.insert(0, sys.argv[1] + "/tools/harness")
from faults import FAULTS
root, name = sys.argv[1], sys.argv[2]
hits = [f for f in FAULTS if f[0] == name]
assert len(hits) == 1, f"unknown fault {name}"
(n, f, old, new) = hits[0]
p = f"{root}/{f}"; s = open(p).read()
assert s.count(old) == 1, f"fault {n}: pattern matches {s.count(old)} times in {f}"
open(p, "w").write(s.replace(old, new, 1))
PY
  then
    echo "$n | PATCH-FAILED (pattern out of date; ci not run)" | tee -a "$OUT"; continue
  fi
  start=$(date +%s)
  (cd "$SCRATCH" && cargo xtask ci) > "$SCRATCH.$n.log" 2>&1
  rc=$?
  failed=$(grep -oE "ci: ✗ [a-z]+" "$SCRATCH.$n.log" | sed 's/ci: ✗ //' | paste -sd, -)
  prot=$(grep -A40 "PROTECTED FILES CHANGED" "$SCRATCH.$n.log" | grep -E "^ci: +(changed|added|removed)" | sed 's/ci: *//' | paste -sd';' -)
  lost=$(grep -oE "REGRESSION: requirements in the floor no longer pass: \[[^]]*\]" "$SCRATCH.$n.log" | head -1)
  echo "$n | ci exit $rc | failed steps: ${failed:-none} | ${lost:-no parity regression} | protected changes: ${prot:-none} | $(( $(date +%s) - start ))s" | tee -a "$OUT"
done
rm -rf "$SCRATCH" "$CARGO_TARGET_DIR"
