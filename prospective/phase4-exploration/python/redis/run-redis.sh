#!/usr/bin/env bash
# Round-3 exploration ladder for redis-py 8.1.0 (4 units x B1/B2/B3/M1).
#
# Requires an already-running loopback Redis (the container is started and
# removed by the caller; see logs/run.*/environment.txt for the exact image,
# digest and docker command).  One fresh process per (mode, unit, level) so a
# crashed level cannot take the rest of the ladder with it.
#
# usage: ./run-redis.sh --port 32851 --logdir logs/run.XXXXXX
set -uo pipefail
STATUS=0

HERE="$(cd "$(dirname "$0")" && pwd)"
PY="${PY:-/home/ava/dropwise-py-work-r3/venv-redis313/bin/python}"
PORT=""
LOGDIR=""
REPS=3

while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --logdir) LOGDIR="$2"; shift 2 ;;
    --reps) REPS="$2"; shift 2 ;;
    *) echo "unknown arg $1" >&2; exit 2 ;;
  esac
done
[ -n "$PORT" ] || { echo "--port required" >&2; exit 2; }
[ -n "$LOGDIR" ] || { echo "--logdir required" >&2; exit 2; }
mkdir -p "$LOGDIR"

cd "$HERE"
for mode in pool direct; do
  for unit in tx set; do
    for level in B1 B2 B3 M1; do
      name="$mode-$unit-$level"
      echo "=== $name $(date -Is) ==="
      timeout 600 "$PY" -m cases.redis_case \
        --host 127.0.0.1 --port "$PORT" --mode "$mode" --unit "$unit" \
        --levels "$level" --reps "$REPS" --round 3 \
        > "$LOGDIR/level-$name.log" 2> "$LOGDIR/level-$name.err"
      rc=$?
      if [ "$rc" -ne 0 ]; then STATUS=1; fi
      echo "$rc" > "$LOGDIR/level-$name.status"
      tail -n 2 "$LOGDIR/level-$name.log"
    done
  done
done
echo "=== ladder done $(date -Is) ==="

exit "$STATUS"
