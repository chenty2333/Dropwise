#!/usr/bin/env bash
# Small interruption/composition experiments; no historical runs are overwritten.
# Requires Docker, the existing postgres:18-alpine image, and cached locked crates.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
container=''
cleanup() {
    if [[ -n "$container" ]]; then
        docker stop --timeout 3 "$container" >/dev/null ||
            echo "Could not stop temporary PostgreSQL container: $container" >&2
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

container=$(docker run --pull never --rm -d \
    -p 127.0.0.1::5432 \
    -e POSTGRES_USER=dropwise \
    -e POSTGRES_PASSWORD=dropwise \
    -e POSTGRES_DB=dropwise \
    postgres:18-alpine)

ready=0
for ((attempt=0; attempt<100; attempt++)); do
    if docker exec "$container" pg_isready -h 127.0.0.1 -U dropwise -d dropwise >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.1
done
if [[ "$ready" != 1 ]]; then
    echo 'Temporary PostgreSQL did not become ready' >&2
    exit 1
fi
port=$(docker port "$container" 5432/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}')
[[ -n "$port" ]] || { echo 'Missing loopback PostgreSQL port' >&2; exit 1; }
export DATABASE_URL="postgres://dropwise:dropwise@127.0.0.1:$port/dropwise?sslmode=disable"

run_test() {
    local crate=$1 test=$2
    for repetition in 1 2 3; do
        printf '\n=== %s, repetition %s/3 ===\n' "$test" "$repetition"
        timeout 180s cargo test --offline --locked \
            --manifest-path "$ROOT/prospective/$crate/Cargo.toml" \
            --test "$test" -- --nocapture --test-threads=1
    done
}

# Preserve the requested priority order. All results go to stdout, not to the
# preregistered studies' runs/ directories. A failure stops subsequent groups.
run_test t3-sqlx observation_limits
run_test t3-sqlx survivors
run_test t2-bb8 handoff
run_test t3-sqlx commit_reply
run_test t3-sqlx pending_window
