#!/usr/bin/env bash
# kill -9 crash test: writer commits continuously and prints "ACK <id>" after each durable commit;
# we kill it at a random time, then verify integrity and that every acked stroke is present.
# Usage: crash.sh <sqlite|redb|blob> <iterations>
set -u
kind=$1; n=$2; dir=data/crash-$kind
rm -rf $dir; mkdir -p $dir
ok=0; fail=0
for i in $(seq $n); do
  if [[ $kind == blob ]]; then ./target/release/st blobw $dir > $dir/acks 2>/dev/null & else ./target/release/st writer $kind $dir > $dir/acks 2>/dev/null & fi
  pid=$!
  sleep $(python3 -c "import random;print(random.uniform(0.02,0.4))")
  kill -9 $pid; wait $pid 2>/dev/null
  if [[ $kind == blob ]]; then
    ./target/release/st blobv $dir $dir/acks > $dir/verify.log 2>&1 && ok=$((ok+1)) || { fail=$((fail+1)); echo "iter $i FAIL: $(cat $dir/verify.log)"; }
  else
    last=$(grep "^ACK" $dir/acks | tail -1 | cut -d" " -f2); last=${last:--1}
    # acks accumulate across iterations: the writer resumes from max id, so "all ≤ last" covers every prior ack
    ./target/release/st verify $kind $dir $last > $dir/verify.log 2>&1 && ok=$((ok+1)) || { fail=$((fail+1)); echo "iter $i FAIL (ack $last): $(cat $dir/verify.log)"; }
  fi
done
echo "$kind: iterations=$n ok=$ok fail=$fail final_db=$(du -sh $dir | cut -f1)"
rm -rf $dir
