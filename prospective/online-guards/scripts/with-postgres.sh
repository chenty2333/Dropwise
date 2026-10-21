#!/usr/bin/env bash
# Run a command against a disposable, loopback-only PostgreSQL 18 instance.
set -euo pipefail
name="og-pg-$$"
cid=$(docker run -d --rm --name "$name" -e POSTGRES_HOST_AUTH_METHOD=trust \
  -e POSTGRES_PASSWORD=x -p 127.0.0.1::5432 postgres:18-alpine)
cleanup() { docker rm -f "$cid" >/dev/null 2>&1 || true; }
trap cleanup EXIT
for _ in $(seq 1 300); do
  docker exec "$cid" pg_isready -U postgres >/dev/null 2>&1 && break
  sleep 0.1
done
sleep 1
port=$(docker port "$cid" 5432/tcp 2>/dev/null | head -1 | sed 's/.*://')
if [ -z "$port" ]; then
  echo "postgres did not start" >&2
  docker logs "$cid" 2>&1 | tail -5 >&2
  exit 1
fi
PG_PORT="$port" "$@"
