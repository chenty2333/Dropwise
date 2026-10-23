#!/usr/bin/env bash
# Pool-ACQUIRE cancellation (not query cancellation) for:
#   psycopg_pool 3.3.3 AsyncConnectionPool.getconn()
#   asyncpg 0.32.0 Pool.acquire()
# against a disposable PostgreSQL 18.4 on loopback (ephemeral host port).
#
# New log directory only.  Existing logs and reports are never touched.
# The tested pool has max_size=1 and holds its only connection in setup;
# the cut target is the second acquire.  An independent direct connection
# observes pg_stat_activity with a separate application_name.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$REPO/.venv/bin/python}
NAME="dw-pa-pg-$$"
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
export PYTHONPATH=".:$REPO/research/python-retro"

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

{
  echo "== environment =="
  date -Iseconds
  "$PY" -V
  echo "git_head=$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
  "$PY" - <<'PY'
import asyncpg, psycopg, psycopg_pool, sys
print(f"python={sys.version.split()[0]}")
print(f"psycopg={psycopg.__version__}")
print(f"psycopg_pool={psycopg_pool.__version__}")
print(f"asyncpg={asyncpg.__version__}")
PY
  "$PY" -m pip freeze 2>/dev/null \
    | grep -Ei '^(psycopg|psycopg-pool|asyncpg)==' || true
  docker image inspect --format \
    'postgres:18-alpine repo_digest={{index .RepoDigests 0}} image_id={{.Id}}' \
    postgres:18-alpine
  docker inspect --format \
    'container={{.Name}} image={{.Config.Image}} image_id={{.Image}}' "$NAME"
  docker exec "$NAME" postgres -V
  echo "port=$PORT run=$RUN"
  echo
  echo "== installed mechanism sources (file:line) =="
  SITE=$("$PY" - <<'PY'
import os, psycopg_pool
print(os.path.dirname(psycopg_pool.__file__))
PY
)
  ASITE=$("$PY" - <<'PY'
import os, asyncpg
print(os.path.dirname(asyncpg.__file__))
PY
)
  echo "-- psycopg_pool/pool_async.py --"
  grep -nE \
    'CLIENT_EXCEPTIONS =|async def getconn|async def _getconn_unchecked|self\._waiting\.append|except CLIENT_EXCEPTIONS:|if pos\.conn|async def _add_to_pool|await self\._waiting\.popleft\(\)\.set|async def putconn|async def _putconn' \
    "$SITE/pool_async.py"
  echo "-- psycopg_pool/pool_async.py WaitingClient --"
  grep -nE 'class WaitingClient|async def wait\(|async def set\(' "$SITE/pool_async.py"
  echo "-- asyncpg/pool.py --"
  grep -nE \
    'async def _acquire\(|self\._queue\.get\(\)|except \(Exception, asyncio\.CancelledError\)|async def release\(|_release_on_close|_schedule_min_size_maintenance|def get_size|def get_idle_size' \
    "$ASITE/pool.py"
  echo "-- asyncpg/pool.py PoolConnectionHolder --"
  grep -nE 'async def acquire\(|async def release\(|def _release\(|self\._pool\._queue\.put_nowait' \
    "$ASITE/pool.py"
  echo
  echo "== commands =="
  cat <<CMDS
cd $PWD
PYTHONPATH=".:$REPO/research/python-retro" $PY -m pool_acquire.psycopg_acquire_case --port $PORT --reps 5
PYTHONPATH=".:$REPO/research/python-retro" $PY -m pool_acquire.asyncpg_acquire_case --port $PORT --reps 5
CMDS
} > "$RUN/environment.txt" 2>&1

echo "== psycopg_pool 3.3.3 acquire ==" \
  > "$RUN/psycopg-pool-acquire.txt"
"$PY" -m pool_acquire.psycopg_acquire_case \
  --port "$PORT" --user postgres --password dw --dbname dw --reps 5 \
  >> "$RUN/psycopg-pool-acquire.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/psycopg-pool-acquire.txt"

echo "== asyncpg 0.32.0 acquire ==" \
  > "$RUN/asyncpg-pool-acquire.txt"
"$PY" -m pool_acquire.asyncpg_acquire_case \
  --port "$PORT" --user postgres --password dw --dbname dw --reps 5 \
  >> "$RUN/asyncpg-pool-acquire.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/asyncpg-pool-acquire.txt"

# No-service contract/static checks (do not talk to the DB).
{
  ( set -e
    "$PY" -m py_compile pool_acquire/support.py \
      pool_acquire/psycopg_acquire_case.py \
      pool_acquire/asyncpg_acquire_case.py
    echo "py_compile ok"
  )
} > "$RUN/compile.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/compile.txt"

{
  for f in psycopg-pool-acquire asyncpg-pool-acquire; do
    echo "$f executions=$(grep -c '^EXEC ' "$RUN/$f.txt" || true) \
clean=$(grep -c '"clean": true' "$RUN/$f.txt" || true) \
records_with_issues=$(grep -c '"issues": \["' "$RUN/$f.txt" || true) \
shapes=$(grep -c '^SHAPE ' "$RUN/$f.txt" || true)"
  done
} > "$RUN/execution-counts.txt"

echo "logs: $RUN"
exit "$STATUS"
