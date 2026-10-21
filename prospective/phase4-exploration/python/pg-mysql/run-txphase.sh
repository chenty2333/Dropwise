#!/usr/bin/env bash
# Round-4 transaction-context cancellation run.
# Fresh disposable PostgreSQL on loopback; every output goes to a new
# logs/run.XXXXXX/ directory.  Old logs are never touched.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r4-txphase-$$"
RUN=$(mktemp -d logs/run.XXXXXX)
RUN_ABS=$(cd "$RUN" && pwd)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT

docker run -d --rm --name "$NAME" \
  -e POSTGRES_PASSWORD=dw -e POSTGRES_DB=dw \
  -p "127.0.0.1::5432" postgres:18-alpine >/dev/null || exit 1
READY=0
for _ in $(seq 1 60); do
  docker exec "$NAME" pg_isready -h 127.0.0.1 -U postgres -d dw >/dev/null 2>&1 \
    && { READY=1; break; }
  sleep 0.5
done
if [ "$READY" -ne 1 ]; then
  echo "PostgreSQL TCP server did not become ready" >&2
  exit 1
fi
PORT=$(docker port "$NAME" 5432/tcp | sed 's/.*://')
if [ -z "${PORT:-}" ]; then echo "no mapped port" >&2; exit 1; fi

export PYTHONPATH=".:$REPO/research/python-retro"

{
  echo "== environment =="
  date -Iseconds
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'psycopg|asyncpg' || true
  docker image inspect --format 'postgres:18-alpine {{index .RepoDigests 0}}' postgres:18-alpine
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  echo "port=$PORT run=$RUN"
} > "$RUN_ABS/environment.txt" 2>&1

run_one() {
  local out="$1"; shift
  "$PY" -m cases.txphase_case --port "$PORT" "$@" > "$RUN_ABS/$out" 2>&1
  local rc=$?
  echo "exit=$rc" >> "$RUN_ABS/$out"
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
}

"$PY" -m unittest tests.test_txphase -v > "$RUN_ABS/harness-tests.txt" 2>&1
rc=$?
echo "exit=$rc" >> "$RUN_ABS/harness-tests.txt"
if [ "$rc" -ne 0 ]; then STATUS=1; fi

# K first; phase probes cover exactly the realised range.
run_one psycopg-count.txt --driver psycopg --count
run_one asyncpg-count.txt --driver asyncpg --count

for d in psycopg asyncpg; do
  if [ "$d" = psycopg ]; then KS=1,2,3,4,5,6; else KS=1,2,3; fi
  run_one "$d-probe-phase.txt" --driver "$d" --probe-phase "$KS"
  run_one "$d-probe-double.txt" --driver "$d" --probe-phase "$KS" --double-cancel
  run_one "$d-probe-reconnect.txt" --driver "$d" --probe-phase 1,2 --recovery reconnect
  if [ "$d" = psycopg ]; then
    run_one "$d-probe-rollback.txt" --driver "$d" --probe-phase 1,2 --recovery rollback
    run_one "$d-probe-raw-rollback.txt" --driver "$d" --probe-phase 1,2 --recovery raw_rollback
  else
    run_one "$d-probe-rollback.txt" --driver "$d" --probe-phase 1,2 --recovery rollback
    run_one "$d-probe-reset.txt" --driver "$d" --probe-phase 1,2 --recovery reset
  fi
  run_one "$d-ladder-none.txt" --driver "$d" --levels B1,B2,B3,M1 --reps 3
  if [ "$d" = psycopg ]; then
    run_one "$d-ladder-recovery.txt" --driver "$d" --levels M1 --reps 2 --recovery reconnect
    run_one "$d-probe-fix.txt" --driver "$d" --probe-phase 1,2 --fix-enter
    run_one "$d-ladder-fix.txt" --driver "$d" --levels M1 --reps 2 --fix-enter
  else
    run_one "$d-ladder-recovery.txt" --driver "$d" --levels M1 --reps 2 --recovery reset
    run_one "$d-probe-fix.txt" --driver "$d" --probe-phase 1 --fix-enter
  fi
done

echo "logs: $RUN"
exit "$STATUS"
