#!/usr/bin/env bash
# Contrast-only rerun (official transaction CM vs manual BEGIN/COMMIT,
# rollback/reset, close+reconnect, ordering/server samples) after moving the
# server sample after the release so a diagnostic cannot strand a pool checkout.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$REPO/.venv/bin/python}
NAME="dw-r3fix-ct-$$"
RUN=$(mktemp -d logs/run.XXXXXX)
RUN_ABS=$(cd "$RUN" && pwd)
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
{
  echo "== environment =="
  date -Iseconds
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'psycopg|asyncpg' || true
  docker image inspect --format 'postgres:18-alpine {{index .RepoDigests 0}}' postgres:18-alpine
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  echo "port=$PORT run=$RUN"
} > "$RUN_ABS/environment.txt" 2>&1

"$PY" -m cases.psycopg_case --port "$PORT" --user postgres --password dw \
  --mode direct --contrast --plans 1,2,3,4,5,6 > "$RUN_ABS/psycopg-direct-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN_ABS/psycopg-direct-contrast.txt"
"$PY" -m cases.psycopg_case --port "$PORT" --user postgres --password dw \
  --mode pool --contrast --plans 1,3,5 > "$RUN_ABS/psycopg-pool-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN_ABS/psycopg-pool-contrast.txt"
"$PY" -m cases.asyncpg_case --port "$PORT" --user postgres --password dw \
  --mode direct --contrast --plans 1,2,3 > "$RUN_ABS/asyncpg-direct-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN_ABS/asyncpg-direct-contrast.txt"
"$PY" -m cases.asyncpg_case --port "$PORT" --user postgres --password dw \
  --mode pool --contrast --plans 1,2,3,4 > "$RUN_ABS/asyncpg-pool-contrast.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN_ABS/asyncpg-pool-contrast.txt"

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
