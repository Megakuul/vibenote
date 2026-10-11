#!/usr/bin/env bash
# Robustness: open + render first 5 pages of each mutated PDF in a subprocess, 10 s timeout.
export PDFIUM_DIR=/nix/store/cw12h22zwsd3cc6k185bcrkjywm5yimb-pdfium-8066/lib
for lib in pdfium hayro; do
  declare -A c=()
  for f in fuzz/*.pdf; do
    timeout 10 ./target/release/pbench $lib one "$f" >/dev/null 2>&1; e=$?
    case $e in 0) k=ok;; 1) k=clean_error;; 101) k=rust_panic;; 124) k=timeout;; 134) k=abort;; 139) k=segfault;; *) k="exit$e";; esac
    c[$k]=$(( ${c[$k]:-0} + 1 ))
    [[ $k != ok && $k != clean_error ]] && echo "$lib $k $f" >> fuzz-failures.txt
  done
  printf "%s:" $lib; for k in "${!c[@]}"; do printf " %s=%s" $k ${c[$k]}; done; echo
  unset c
done
