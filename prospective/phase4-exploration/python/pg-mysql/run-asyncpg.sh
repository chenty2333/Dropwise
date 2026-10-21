#!/usr/bin/env bash
# asyncpg 0.32.0: pool and direct units, all four levels.
# Disposable Postgres on loopback, unique container name, unique log dir.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r3-py-apg-$$"
PORT=${PGPORT:-55432}
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
export PYTHONPATH=".:$REPO/research/python-retro"

docker run -d --rm --name "$NAME" \
  -e POSTGRES_PASSWORD=dw -e POSTGRES_DB=dw \
  -p "127.0.0.1:$PORT:5432" postgres:18-alpine >/dev/null || exit 1
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

{
  echo "== environment =="
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'psycopg|asyncpg|aiomysql|mysql.connector|pymysql' || true
  docker image inspect --format 'postgres:18-alpine {{index .RepoDigests 0}}' postgres:18-alpine
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  echo "port=$PORT run=$RUN"
} > "$RUN/environment.txt" 2>&1

for mode in pool direct; do
  "$PY" -m cases.asyncpg_case --port "$PORT" --user postgres --password dw \
    --mode "$mode" --levels B1,B2,B3,M1 --reps 3 > "$RUN/asyncpg-$mode.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/asyncpg-$mode.txt"
done
echo "logs: $RUN"

exit "$STATUS"
