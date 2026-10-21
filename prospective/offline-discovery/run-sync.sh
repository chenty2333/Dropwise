#!/usr/bin/env bash
# Reproduce the offline-discovery sync-interruption experiments.
#
#   bash run-sync.sh [output-dir]
#
# Starts a throwaway PostgreSQL container bound to 127.0.0.1 on an ephemeral
# port, runs the pinned-point check, the no-service tests, the timing sweep,
# the original/fix phase matrices, the pool tests, the real-SIGINT check, the
# legitimate-vs-abandoned contrast, and the field-contract replay.  The
# container is removed on exit.  Requires docker and the Python environment
# named by $PYTHON (default: ~/dropwise-py-work-r3/venv313/bin/python).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
PY="${PYTHON:-$HOME/dropwise-py-work-r3/venv313/bin/python}"
RUN="${1:-$ROOT/logs/run-$(date +%Y%m%d-%H%M%S)}"
IMAGE="${IMAGE:-postgres:18-alpine}"
NAME="dw-od-run-$$"

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
[ -x "$PY" ] || { echo "python not found: $PY (set PYTHON=...)" >&2; exit 1; }
mkdir -p "$RUN"

echo "run dir: $RUN"
docker run -d --name "$NAME" \
    -e POSTGRES_PASSWORD=dw -e POSTGRES_DB=dw \
    -p 127.0.0.1::5432 "$IMAGE" >/dev/null
for _ in $(seq 1 60); do
    if docker exec "$NAME" pg_isready -U postgres -d dw >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done
PORT="$(docker port "$NAME" 5432/tcp | head -n1 | sed 's/.*://')"
[ -n "$PORT" ] || { echo "could not determine container port" >&2; exit 1; }
echo "postgres: 127.0.0.1:$PORT"

cd "$ROOT/python"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" points | tee "$RUN/points.txt"
"$PY" -m unittest discover -s pgsync/tests -v >"$RUN/unit-sync.txt" 2>&1
( cd "$ROOT/../../research/python-retro" && \
  "$PY" -m unittest tests.test_fields -v >"$RUN/unit-fields.txt" 2>&1 )

echo "sweep"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" sweep \
    --offsets-us 2,5,10,15,20,25,30,40,60,100,200,500 --reps 5 --mode survivor \
    >"$RUN/sweep.jsonl"

echo "phases (original)"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" phases \
    --reps 5 --calib 2 >"$RUN/phases2.jsonl"

echo "phases (fix)"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" fix \
    --reps 5 --calib 2 >"$RUN/fix2.jsonl"

echo "pool"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" pool \
    --reps 3 >"$RUN/pool.jsonl"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" pool \
    --reps 3 --fix >"$RUN/poolfix.jsonl"

echo "real SIGINT"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" sigint \
    --reps 3 >"$RUN/sigint.jsonl"

echo "contrast"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" contrast \
    >"$RUN/contrast.jsonl"

echo "field contract on the sync target"
"$PY" -m pgsync.case --port "$PORT" --log-dir "$RUN" fields \
    --phases entry_wait,entry_pre_send,exit_pre_commit,entry_normal --reps 2 \
    >"$RUN/fields.jsonl"

"$PY" -m pgsync.analyze --dir "$RUN" >"$RUN/SUMMARY.md"

echo
echo "done: $RUN/SUMMARY.md"
echo "unit tests: $RUN/unit-sync.txt $RUN/unit-fields.txt"
echo "raw records: sweep.jsonl phases2.jsonl fix2.jsonl pool.jsonl poolfix.jsonl"
echo "             sigint.jsonl contrast.jsonl fields.jsonl"
