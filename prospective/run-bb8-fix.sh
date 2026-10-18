#!/usr/bin/env bash
# bb8 is_valid cancellation (RESCAN.md, section 4). Results go to stdout only.
#
# 1. The frozen T2 scenarios (S2.1-S2.5 with their positive control) and the T2
#    rescan, against bb8 0.9.1 with patches/bb8-invalid-until-validated.diff.
# 2. Published managers (bb8-redis, bb8-postgres, bb8-memcached) against real
#    loopback servers, with crates.io bb8 and with the same patch applied to
#    bb8 0.9.1 and 0.8.6 (bb8-memcached still uses bb8 0.8).
# 3. deadpool 0.13.1 for comparison: the same request/response model, and
#    deadpool-memcached 0.5.0 against memcached (RESCAN.md, section 5).
#
# Patched copies live in $DROPWISE_RESCAN_WORK (default ~/dropwise-rescan-work).
# Requires Docker with local redis:8-alpine, postgres:18-alpine and
# memcached:1.6-alpine images, and cached crates.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=${DROPWISE_RESCAN_WORK:-$HOME/dropwise-rescan-work}
REPS=${REPS:-3}
PATCH="$ROOT/prospective/t2-bb8/patches/bb8-invalid-until-validated.diff"

registry_crate() {
    local dir
    dir=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$1" | head -n1)
    [[ -n "$dir" ]] || { echo "$1 is not in the cargo registry cache" >&2; exit 1; }
    echo "$dir"
}

patched_bb8() { # version -> path of a patched copy
    rm -rf "$WORK/bb8-$1-fixed"
    cp -r "$(registry_crate "bb8-$1")" "$WORK/bb8-$1-fixed"
    patch -d "$WORK/bb8-$1-fixed" -p1 --quiet < "$PATCH"
    echo "$WORK/bb8-$1-fixed"
}

copy_crate() { # crate dir name -> path of a copy whose dropwise path is absolute
    rm -rf "$WORK/$1-fixed"
    mkdir -p "$WORK/$1-fixed"
    tar -C "$ROOT/prospective/$1" --exclude=./target -cf - . | tar -C "$WORK/$1-fixed" -xf -
    sed -i "s#path = \"../..\"#path = \"$ROOT\"#" "$WORK/$1-fixed/Cargo.toml"
    echo "$WORK/$1-fixed"
}

run() { # manifest dir, label, cargo test args...
    local dir=$1 label=$2
    for repetition in $(seq "$REPS"); do
        printf '\n=== %s, repetition %s/%s ===\n' "$label" "$repetition" "$REPS"
        timeout 300s cargo test --offline "${@:3}" --manifest-path "$dir/Cargo.toml" -- --nocapture --test-threads=1
    done
}

mkdir -p "$WORK"
rm -rf "$WORK/support"
cp -r "$ROOT/prospective/support" "$WORK/support"
bb8_09=$(patched_bb8 0.9.1)
bb8_08=$(patched_bb8 0.8.6)

# 1. Frozen T2 scenarios and the T2 rescan. The positive-control mutant is also
#    a path crate named bb8 0.9.1; renumber it in the copy so the two can coexist.
t2=$(copy_crate t2-bb8)
sed -i '0,/^version = "0.9.1"/s//version = "0.9.2"/' "$t2/vendor/bb8-mutant/Cargo.toml"
printf '\n[patch.crates-io]\nbb8 = { path = "%s" }\n' "$bb8_09" >> "$t2/Cargo.toml"
BB8_VARIANT=fixed run "$t2" 'T2 frozen + rescan, fixed bb8' --test scenarios --test rescan

# 2. Published managers against real servers.
containers=()
cleanup() {
    if ((${#containers[@]})); then
        docker stop --timeout 3 "${containers[@]}" >/dev/null ||
            echo "Could not stop temporary containers: ${containers[*]}" >&2
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

containers+=("$(docker run --pull never --rm -d -p 127.0.0.1::6379 redis:8-alpine)")
containers+=("$(docker run --pull never --rm -d -p 127.0.0.1::5432 \
    -e POSTGRES_USER=dropwise -e POSTGRES_PASSWORD=dropwise -e POSTGRES_DB=dropwise postgres:18-alpine)")
containers+=("$(docker run --pull never --rm -d -p 127.0.0.1::11211 memcached:1.6-alpine)")
host_port() { docker port "$1" "$2/tcp" | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}'; }
export DW_REDIS_PORT DW_PG_PORT DW_MEMCACHED_PORT
DW_REDIS_PORT=$(host_port "${containers[0]}" 6379)
DW_PG_PORT=$(host_port "${containers[1]}" 5432)
DW_MEMCACHED_PORT=$(host_port "${containers[2]}" 11211)
ready=0
for ((attempt=0; attempt<100; attempt++)); do
    if docker exec "${containers[1]}" pg_isready -h 127.0.0.1 -U dropwise -d dropwise >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.1
done
[[ "$ready" == 1 ]] || { echo 'Temporary PostgreSQL did not become ready' >&2; exit 1; }

BB8_VARIANT=release run "$ROOT/prospective/t2-bb8-managers" 'managers, crates.io bb8' --locked --test managers
managers=$(copy_crate t2-bb8-managers)
printf '\n[patch.crates-io]\nbb8 = { path = "%s" }\nbb8_08 = { package = "bb8", path = "%s" }\n' \
    "$bb8_09" "$bb8_08" >> "$managers/Cargo.toml"
BB8_VARIANT=fixed run "$managers" 'managers, fixed bb8' --test managers
run "$ROOT/prospective/t2-bb8-managers" 'deadpool' --locked --test deadpool
