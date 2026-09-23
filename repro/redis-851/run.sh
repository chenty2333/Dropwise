#!/bin/sh
# Start a throwaway Redis (local image), run the test against it, remove it.
set -eu
cd "$(dirname "$0")"
id=$(docker run -d --rm -p 127.0.0.1::6379 redis:8-alpine)
trap 'docker stop "$id" >/dev/null' EXIT
port=$(docker port "$id" 6379/tcp | head -1 | sed 's/.*://')
for _ in $(seq 50); do docker exec "$id" redis-cli ping >/dev/null 2>&1 && break; sleep 0.1; done
REDIS_URL="redis://127.0.0.1:$port" cargo test "$@"
