#!/bin/sh
# Run T3 three times against a throwaway, loopback-only Postgres instance.
set -u
cd "$(dirname "$0")/.."
mkdir -p runs/logs

id=$(docker run -d --rm \
  -p 127.0.0.1::5432 \
  -e POSTGRES_USER=dropwise \
  -e POSTGRES_PASSWORD=dropwise \
  -e POSTGRES_DB=dropwise \
  postgres:18-alpine)
stop_db() { docker stop "$id" >/dev/null 2>&1 || true; }
trap stop_db EXIT HUP INT TERM

port=$(docker port "$id" 5432/tcp | head -1 | sed 's/.*://')
ready=0
for _ in $(seq 50); do
  if docker exec "$id" pg_isready -U dropwise -d dropwise >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.1
done
if [ "$ready" -ne 1 ]; then
  echo "Postgres did not become ready" >&2
  exit 1
fi

status=0
for n in 1 2 3; do
  log="runs/logs/t3-sqlx-$n.log"
  if (cd t3-sqlx && DATABASE_URL="postgres://dropwise:dropwise@127.0.0.1:$port/dropwise" RUN=$n cargo test -q -- --test-threads 1 --nocapture) >"$log" 2>&1; then
    echo "T3 run $n: complete (full output: prospective/$log)"
  else
    rc=$?
    echo "T3 run $n: cargo test exited $rc (full output: prospective/$log)"
    status=1
  fi
done
exit "$status"
