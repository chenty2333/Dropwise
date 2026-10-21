#!/usr/bin/env bash
# Corrected rerun for psycopg 3.3.6 / asyncpg 0.32.0 after the OpResult adapter fix.
# New log directory only; the old logs/run.KqRbB4/ is never touched.
# Disposable PostgreSQL on loopback, ephemeral host port, removed on exit.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r3fix-pg-$$"
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
export PYTHONPATH=".:$REPO/research/python-retro"

docker run -d --rm --name "$NAME" \
  -e POSTGRES_PASSWORD=dw -e POSTGRES_DB=dw \
  -p "127.0.0.1::5432" postgres:18-alpine >/dev/null || exit 1
# initdb's temporary server accepts Unix sockets before TCP is ready.
READY=0
for _ in $(seq 1 60); do
  docker exec "$NAME" pg_isready -h 127.0.0.1 -U postgres -d dw >/dev/null 2>&1 && { READY=1; break; }
  sleep 0.5
done
if [ "$READY" -ne 1 ]; then
  echo "PostgreSQL TCP server did not become ready" >&2
  exit 1
fi
PORT=$(docker port "$NAME" 5432/tcp | sed 's/.*://')
if [ -z "${PORT:-}" ]; then echo "no mapped port" >&2; exit 1; fi

{
  echo "== environment =="
  date -Iseconds
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'psycopg|asyncpg|aiomysql|mysql.connector|pymysql' || true
  docker image inspect --format 'postgres:18-alpine {{index .RepoDigests 0}}' postgres:18-alpine
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  echo "port=$PORT run=$RUN"
  echo "== asyncio.wait_for task shape (3.13 normal path) =="
  "$PY" - <<'PY'
import asyncio
async def inner():
    return id(asyncio.current_task())
async def outer():
    me = id(asyncio.current_task())
    child = await asyncio.wait_for(inner(), 1)
    print(f"outer_task={me} inner_task={child} child_task={me != child}")
asyncio.run(outer())
PY
} > "$RUN/environment.txt" 2>&1

for mode in pool direct; do
  "$PY" -m cases.psycopg_case --port "$PORT" --user postgres --password dw \
    --mode "$mode" --levels B1,B2,B3,M1 --reps 3 \
    > "$RUN/psycopg-$mode-fixed.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/psycopg-$mode-fixed.txt"

done
for mode in pool direct; do
  "$PY" -m cases.asyncpg_case --port "$PORT" --user postgres --password dw \
    --mode "$mode" --levels B1,B2,B3,M1 --reps 3 \
    > "$RUN/asyncpg-$mode-fixed.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/asyncpg-$mode-fixed.txt"
done

# Boundary/ordering contrasts.  psycopg plans 1-6; asyncpg direct 1-3, pool 1-4.
"$PY" -m cases.psycopg_case --port "$PORT" --user postgres --password dw \
  --mode direct --contrast --plans 1,2,3,4,5,6 > "$RUN/psycopg-direct-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/psycopg-direct-contrast.txt"
"$PY" -m cases.psycopg_case --port "$PORT" --user postgres --password dw \
  --mode pool --contrast --plans 1,3,5 > "$RUN/psycopg-pool-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/psycopg-pool-contrast.txt"
"$PY" -m cases.asyncpg_case --port "$PORT" --user postgres --password dw \
  --mode direct --contrast --plans 1,2,3 > "$RUN/asyncpg-direct-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/asyncpg-direct-contrast.txt"
"$PY" -m cases.asyncpg_case --port "$PORT" --user postgres --password dw \
  --mode pool --contrast --plans 1,2,3,4 > "$RUN/asyncpg-pool-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/asyncpg-pool-contrast.txt"

# No-service regression tests (adapter + lead engine tests).
RUN_ABS=$(cd "$RUN" && pwd)
{
  ( set -e
    cd "$REPO"
    PYTHONPATH=research/python-retro "$PY" -m unittest discover \
      -s prospective/phase4-exploration/python/pg-mysql/tests -v
    PYTHONPATH=research/python-retro "$PY" -m unittest discover \
      -s research/python-retro/tests -v
  )
} > "$RUN_ABS/tests.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN_ABS/tests.txt"

echo "logs: $RUN"

exit "$STATUS"
