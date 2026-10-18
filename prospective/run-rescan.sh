#!/usr/bin/env bash
# Post-hoc rescan of T1-T3 (see RESCAN.md). Results go to stdout only; the
# frozen study's runs/ directory is not touched.
#
# T3 runs twice: against crates.io sqlx 0.9.0, and against the same version
# with the upstream fix for sqlx#4393 (patches/sqlx-pr4394-transaction.diff)
# applied to a copy of sqlx-postgres in $DROPWISE_RESCAN_WORK (default
# ~/dropwise-rescan-work). Requires Docker, the local postgres:18-alpine image
# and cached crates; starts a loopback-only PostgreSQL and stops it on exit.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=${DROPWISE_RESCAN_WORK:-$HOME/dropwise-rescan-work}
REPS=${REPS:-3}

rescan() {
    local dir=$1 label=$2
    for repetition in $(seq "$REPS"); do
        printf '\n=== %s, repetition %s/%s ===\n' "$label" "$repetition" "$REPS"
        timeout 300s cargo test --offline "${@:3}" --manifest-path "$dir/Cargo.toml" \
            --test rescan -- --nocapture --test-threads=1
    done
}

rescan "$ROOT/prospective/t1-tungstenite" T1 --locked
rescan "$ROOT/prospective/t2-bb8" T2 --locked

# Patched copy of the T3 crate. Its lockfile differs from the original only in
# where sqlx-postgres comes from.
registry=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name 'sqlx-postgres-0.9.0' | head -n1)
[[ -n "$registry" ]] || { echo 'sqlx-postgres 0.9.0 is not in the cargo registry cache' >&2; exit 1; }
rm -rf "$WORK/sqlx-postgres-pr4394" "$WORK/t3-sqlx-pr4394" "$WORK/support"
mkdir -p "$WORK/t3-sqlx-pr4394"
cp -r "$registry" "$WORK/sqlx-postgres-pr4394"
patch -d "$WORK/sqlx-postgres-pr4394" -p1 --quiet < "$ROOT/prospective/t3-sqlx/patches/sqlx-pr4394-transaction.diff"
cp -r "$ROOT/prospective/support" "$WORK/support"
cp -r "$ROOT/prospective/t3-sqlx/"{Cargo.toml,Cargo.lock,src,tests} "$WORK/t3-sqlx-pr4394/"
sed -i "s#path = \"../..\"#path = \"$ROOT\"#" "$WORK/t3-sqlx-pr4394/Cargo.toml"
printf '\n[patch.crates-io]\nsqlx-postgres = { path = "../sqlx-postgres-pr4394" }\n' >> "$WORK/t3-sqlx-pr4394/Cargo.toml"

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
[[ "$ready" == 1 ]] || { echo 'Temporary PostgreSQL did not become ready' >&2; exit 1; }
port=$(docker port "$container" 5432/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}')
[[ -n "$port" ]] || { echo 'Missing loopback PostgreSQL port' >&2; exit 1; }
export DATABASE_URL="postgres://dropwise:dropwise@127.0.0.1:$port/dropwise?sslmode=disable"

SQLX_VARIANT=release rescan "$ROOT/prospective/t3-sqlx" 'T3 release' --locked
SQLX_VARIANT=pr4394 rescan "$WORK/t3-sqlx-pr4394" 'T3 pr4394'
diff <(grep -v '^#' "$ROOT/prospective/t3-sqlx/Cargo.lock") <(grep -v '^#' "$WORK/t3-sqlx-pr4394/Cargo.lock") || true
