#!/usr/bin/env bash
# diesel-async 0.9.2 MySQL behind bb8 (candidate-scan lead). Results go to stdout only.
#
# Runs as released and with both scan-diesel-async/patches/ applied to copies in
# $DROPWISE_RESCAN_WORK (default ~/dropwise-rescan-work). LADDER=1 selects the
# cancellation-ablation ladder (B1/B2/B3/M for DA1-DA5); PROXY=1 selects the
# held-reply proxy variants of DA1 and DA3. Requires Docker with a local
# mariadb:11.4 image and cached crates; the server listens on loopback only and
# is stopped on exit.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=${DROPWISE_RESCAN_WORK:-$HOME/dropwise-rescan-work}
REPS=${REPS:-3}
CRATE=$ROOT/prospective/scan-diesel-async

run() { # manifest dir, label, cargo test args...
    local dir=$1 label=$2
    for repetition in $(seq "$REPS"); do
        printf '\n=== %s, repetition %s/%s ===\n' "$label" "$repetition" "$REPS"
        timeout "${TIMEOUT:-300}s" cargo test --offline "${@:3}" --manifest-path "$dir/Cargo.toml" -- --nocapture --test-threads=1
    done
}

patched() { # crate-version, patch file -> path of a patched copy
    local src
    src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$1" | head -n1)
    [[ -n "$src" ]] || { echo "$1 is not in the cargo registry cache" >&2; exit 1; }
    rm -rf "$WORK/$1-fixed"
    cp -r "$src" "$WORK/$1-fixed"
    patch -d "$WORK/$1-fixed" -p1 --quiet < "$CRATE/patches/$2"
    echo "$WORK/$1-fixed"
}

mkdir -p "$WORK"
rm -rf "$WORK/scan-diesel-async-fixed" "$WORK/support"
# The fix: diesel-async's pooled is_broken also reports a connection that
# mysql_async still marks disconnected (a command dropped midway). mysql_async
# 0.36, which diesel-async 0.9.2 uses, keeps that flag private; 0.37 exposes it
# as Conn::is_disconnected, backported here.
diesel_async=$(patched diesel-async-0.9.2 diesel-async-mysql-is-broken.diff)
mysql_async=$(patched mysql_async-0.36.2 mysql_async-0.36-is-disconnected.diff)
mkdir -p "$WORK/scan-diesel-async-fixed"
tar -C "$CRATE" --exclude=./target -cf - . | tar -C "$WORK/scan-diesel-async-fixed" -xf -
sed -i "s#path = \"../..\"#path = \"$ROOT\"#" "$WORK/scan-diesel-async-fixed/Cargo.toml"
printf '\n[patch.crates-io]\ndiesel-async = { path = "%s" }\nmysql_async = { path = "%s" }\n' \
    "$diesel_async" "$mysql_async" >> "$WORK/scan-diesel-async-fixed/Cargo.toml"
# The ladder test includes ../../support/ladder.rs; the fixed copy sits at the
# same depth under $WORK, so the shared support files go next to it.
cp -r "$ROOT/prospective/support" "$WORK/support"

container=$(docker run --pull never --rm -d -p 127.0.0.1::3306 \
    -e MARIADB_ROOT_PASSWORD=dropwise -e MARIADB_DATABASE=dropwise \
    -e MARIADB_USER=dropwise -e MARIADB_PASSWORD=dropwise mariadb:11.4)
trap 'docker stop --timeout 3 "$container" >/dev/null || echo "Could not stop $container" >&2' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
ready=0
for ((attempt=0; attempt<120; attempt++)); do
    if docker exec "$container" healthcheck.sh --connect --innodb_initialized >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.5
done
[[ "$ready" == 1 ]] || { echo 'Temporary MariaDB did not become ready' >&2; exit 1; }
port=$(docker port "$container" 3306/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}')
export DW_MYSQL_URL="mysql://dropwise:dropwise@127.0.0.1:$port/dropwise"

# LADDER=1 runs the cancellation-ablation ladder (B1/B2/B3/M for DA1-DA5);
# PROXY=1 runs the held-reply variants of DA1 and DA3. Both take longer than
# the plain scan, so each test process gets a longer bound. If both are set,
# LADDER wins.
if [[ "${LADDER:-0}" == 1 ]]; then
    TIMEOUT=900
    tests=(--test mysql_ladder)
elif [[ "${PROXY:-0}" == 1 ]]; then
    TIMEOUT=900
    tests=(--features net --test mysql_proxy)
else
    tests=(--test mysql_bb8 --test mysql_cached)
fi

SCAN_VARIANT=release run "$CRATE" 'release' --locked "${tests[@]}"
SCAN_VARIANT=fixed run "$WORK/scan-diesel-async-fixed" 'fixed' "${tests[@]}"
