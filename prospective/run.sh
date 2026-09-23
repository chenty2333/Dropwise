#!/bin/sh
# Run every scenario of a target 3 times (PREREGISTRATION.md section 4.2).
# Usage: ./run.sh t1-tungstenite | t2-bb8
set -u
cd "$(dirname "$0")"
target=$1
mkdir -p runs/logs
for n in 1 2 3; do
  (cd "$target" && RUN=$n cargo test -q -- --test-threads 1 --nocapture) > "runs/logs/$target-$n.log" 2>&1
  echo "$target run $n: exit $?"
done
