#!/usr/bin/env bash
# Round-4 pool-layer probe: cancelled transaction entry and the pool's
# return path.  Fresh disposable PostgreSQL; new log dir; no existing logs.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$REPO/.venv/bin/python}
NAME="dw-r4-pooltx-$$"
RUN=$(mktemp -d logs/run.pool.XXXXXX)
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
[ "$READY" -eq 1 ] || { echo "PostgreSQL did not become ready" >&2; exit 1; }
PORT=$(docker port "$NAME" 5432/tcp | sed 's/.*://')
[ -n "${PORT:-}" ] || { echo "no mapped port" >&2; exit 1; }

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
  "$PY" -m cases.pool_txphase_case --port "$PORT" "$@" \
    > "$RUN_ABS/$out" 2>&1
  local rc=$?
  echo "exit=$rc" >> "$RUN_ABS/$out"
  [ "$rc" -eq 0 ] || STATUS=1
}

for d in psycopg asyncpg; do
  run_one "$d-pool.txt" --driver "$d" --kinds baseline,propagate,swallow,retry_same --reps 3
done

echo "logs: $RUN"
exit "$STATUS"
