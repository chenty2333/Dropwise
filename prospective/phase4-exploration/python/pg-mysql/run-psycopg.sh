#!/usr/bin/env bash
# psycopg 3.3.6 + psycopg-pool 3.3.3: pool and direct units, all four levels.
# Disposable Postgres on loopback, unique container name, unique log dir.
set -u
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r3-py-pg-$$"
PORT=${PGPORT:-55432}
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
export PYTHONPATH=".:$REPO/research/python-retro"

docker run -d --rm --name "$NAME" \
  -e POSTGRES_PASSWORD=dw -e POSTGRES_DB=dw \
  -p "127.0.0.1:$PORT:5432" postgres:18-alpine >/dev/null || exit 1
for _ in $(seq 1 60); do
  docker exec "$NAME" pg_isready -U postgres -d dw >/dev/null 2>&1 && break
  sleep 0.5
done

{
  echo "== environment =="
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'psycopg|asyncpg|aiomysql|mysql.connector|pymysql' || true
  docker image inspect --format 'postgres:18-alpine {{index .RepoDigests 0}}' postgres:18-alpine
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  echo "port=$PORT run=$RUN"
} > "$RUN/environment.txt" 2>&1

for mode in pool direct; do
  "$PY" -m cases.psycopg_case --port "$PORT" --user postgres --password dw \
    --mode "$mode" --levels B1,B2,B3,M1 --reps 3 > "$RUN/psycopg-$mode.txt" 2>&1
  echo "exit=$?" >> "$RUN/psycopg-$mode.txt"
done
echo "logs: $RUN"
