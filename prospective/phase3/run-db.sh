#!/usr/bin/env bash
# Phase 3 database targets: mongodb (official mongo image), mysql_async (local
# mariadb:11.4), clickhouse-arrow (official clickhouse/clickhouse-server image).
#
# Starts one disposable container per target, all published on 127.0.0.1 only,
# and stops them on exit. Each level B1/B2/B3/M/M-residue runs REPS times; the
# script prints the raw LADDER lines and saves them under db-clients/logs/.
#
# Usage:
#   bash prospective/phase3/run-db.sh                    # all targets, release
#   VARIANT=patched bash prospective/phase3/run-db.sh    # patch/usage contrast
#   TARGETS=mysql_async REPS=1 bash ...                  # one target
#
# Environment: REPS (default 3), VARIANT=release|patched (default release),
# TARGETS (default "mysql_async mongodb clickhouse"), TIMEOUT (per test
# process, default 900), DROPWISE_PHASE3_WORK (crate copies for patched runs).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CRATE=$ROOT/prospective/phase3/db-clients
WORK=${DROPWISE_PHASE3_WORK:-$HOME/dropwise-phase3-work}
REPS=${REPS:-3}
VARIANT=${VARIANT:-release}
TARGETS=${TARGETS:-"mysql_async mongodb clickhouse"}
TIMEOUT=${TIMEOUT:-900}
LOG=$CRATE/logs

# Fixed image tags. mysql_async uses the local image; the other two are pulled
# by docker run if absent.
MARIADB_IMAGE=mariadb:11.4
MONGO_IMAGE=mongo:7.0.21
CLICKHOUSE_IMAGE=clickhouse/clickhouse-server:25.5.2.47-alpine

containers=()
cleanup() {
    for container in "${containers[@]}"; do
        docker rm -f "$container" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# docker run's stdout is captured into a global, not into command substitution:
# a subshell would drop the `containers` array entry and leak the container.
LAST_CONTAINER=
start_container() { # name, image, published ports...
    local name=$1 image=$2
    shift 2
    LAST_CONTAINER=$(docker run --rm -d --name "$name" "$@" "$image") || {
        echo "could not start $image" >&2
        exit 1
    }
    containers+=("$LAST_CONTAINER")
}

wait_mysql() {
    local id=$1
    local ready=0
    for ((attempt = 0; attempt < 180; attempt++)); do
        if docker exec "$id" healthcheck.sh --connect --innodb_initialized >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.5
    done
    [[ $ready == 1 ]] || { echo "MariaDB did not become ready" >&2; exit 1; }
    docker port "$id" 3306/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}'
}

wait_mongo() {
    local id=$1
    local ready=0
    for ((attempt = 0; attempt < 180; attempt++)); do
        if docker exec "$id" mongosh --quiet --eval 'db.runCommand({ping:1})' >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.5
    done
    [[ $ready == 1 ]] || { echo "MongoDB did not become ready" >&2; exit 1; }
    # Single-node replica set: transactions and sessions are allowed. The
    # member advertises the in-container port; clients use directConnection.
    docker exec "$id" mongosh --quiet --eval \
        'try { rs.initiate({_id:"rs0",members:[{_id:0,host:"127.0.0.1:27017"}]}) } catch (e) { print(e) }' >/dev/null
    for ((attempt = 0; attempt < 120; attempt++)); do
        if [[ $(docker exec "$id" mongosh --quiet --eval 'rs.status().myState' 2>/dev/null) == 1 ]]; then
            docker port "$id" 27017/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}'
            return
        fi
        sleep 0.5
    done
    echo "MongoDB replica set did not become primary" >&2
    exit 1
}

wait_clickhouse() {
    local id=$1
    local ready=0
    for ((attempt = 0; attempt < 240; attempt++)); do
        if docker exec "$id" clickhouse-client --query 'SELECT 1' >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.5
    done
    [[ $ready == 1 ]] || { echo "ClickHouse did not become ready" >&2; exit 1; }
    docker port "$id" 9000/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}'
}

# A patched crate copy: copy the registry source, apply one diff, and return
# the directory. The scenario crate copy is built separately, with a
# [patch.crates-io] section pointing at each patched dependency.
patched_source() { # crate-version, diff -> path
    local src
    src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$1" | head -n1)
    [[ -n $src ]] || { echo "$1 is not in the cargo registry cache" >&2; return 1; }
    local dest=$WORK/$1-fixed
    rm -rf "$dest"
    cp -r "$src" "$dest"
    patch -d "$dest" -p1 --quiet < "$CRATE/patches/$2"
    echo "$dest"
}

# The patched variant of the whole scenario crate, with the target's patch
# directory applied. Each target may add a patches/<target>/Cargo.patch.toml
# fragment that lists its [patch.crates-io] entries; the scenario crate is
# copied so test files stay identical.
fixed_scenario_copy() { # target -> manifest dir
    local target=$1
    local dest=$WORK/phase3/db-clients-$target-fixed
    rm -rf "$dest"
    mkdir -p "$dest"
    tar -C "$CRATE" --exclude=./target --exclude=./logs -cf - . | tar -C "$dest" -xf -
    sed -i "s#path = \"../../..\"#path = \"$ROOT\"#" "$dest/Cargo.toml"
    if [[ -f $CRATE/patches/$target/Cargo.patch.toml ]]; then
        printf '\n' >> "$dest/Cargo.toml"
        sed "s#@WORK@#$WORK#g" "$CRATE/patches/$target/Cargo.patch.toml" >> "$dest/Cargo.toml"
    fi
    mkdir -p "$WORK/support"
    cp -r "$ROOT/prospective/support" "$WORK/support"
    echo "$dest"
}

run_single() { # target, crate dir, test, repetition, extra env...
    local target=$1 dir=$2 test=$3 repetition=$4
    shift 4
    local out=$LOG/$target/$VARIANT-$repetition.log
    mkdir -p "$LOG/$target"
    printf '\n=== %s %s, repetition %s/%s ===\n' "$target" "$VARIANT" "$repetition" "$REPS"
    if ! env SCAN_VARIANT="$VARIANT" "$@" timeout "${TIMEOUT}s" cargo test --offline --manifest-path "$dir/Cargo.toml" \
        --test "$test" -- --nocapture --test-threads=1 2>&1 | tee "$out"; then
        echo "=== $target $VARIANT repetition $repetition failed (see $out)" >&2
    fi
}

run_one() { # target, crate dir, test name, extra env...
    local target=$1 dir=$2 test=$3
    shift 3
    for repetition in $(seq "$REPS"); do
        run_single "$target" "$dir" "$test" "$repetition" "$@"
    done
}

mysql() {
    start_container "dw-p3-mysql-$$" "$MARIADB_IMAGE" \
        -p 127.0.0.1::3306 \
        -e MARIADB_ROOT_PASSWORD=dropwise -e MARIADB_DATABASE=dropwise \
        -e MARIADB_USER=dropwise -e MARIADB_PASSWORD=dropwise
    local id=$LAST_CONTAINER
    local port
    port=$(wait_mysql "$id")
    local url="mysql://dropwise:dropwise@127.0.0.1:$port/dropwise"
    local dir=$CRATE
    local guard=
    if [[ $VARIANT == patched ]]; then
        # The library patch refuses a new command on a connection whose
        # previous routine was dropped; the guard is the caller's part: check
        # Conn::is_disconnected(), then replace the connection. Both are
        # needed, and both are what the next caller has to do.
        patched_source mysql_async-0.37.1 mysql_async/mysql_async-0.37.1-connection-closed.diff >/dev/null
        dir=$(fixed_scenario_copy mysql_async)
        guard="DW_MYSQL_GUARD=1"
    fi
    mkdir -p "$WORK/phase3"
    run_one mysql_async "$dir" mysql_async DW_MYSQL_URL="$url" ${guard:+"$guard"}
}

mongodb() {
    local dir=$CRATE
    if [[ $VARIANT == patched ]]; then
        patched_source mongodb-3.9.1 mongodb/mongodb-3.9.1-cancelled-transaction-abort.diff >/dev/null
        dir=$(fixed_scenario_copy mongodb)
    fi
    # A cancelled MG1 leaves an open server transaction. A per-run database
    # drop would wait on its lock and keep blocking later runs, so the test
    # does not drop databases; a fresh server per repetition clears them.
    for repetition in $(seq "$REPS"); do
        local id port url
        # --replSet/--bind_ip_all are the image's command, so they must follow
        # the image name; start_container only takes docker options.
        LAST_CONTAINER=$(docker run --rm -d --name "dw-p3-mongo-$$-$repetition" \
            -p 127.0.0.1::27017 "$MONGO_IMAGE" --replSet rs0 --bind_ip_all) || {
            echo "could not start $MONGO_IMAGE" >&2
            exit 1
        }
        containers+=("$LAST_CONTAINER")
        id=$LAST_CONTAINER
        port=$(wait_mongo "$id")
        url="mongodb://127.0.0.1:$port/?directConnection=true"
        run_single mongodb "$dir" mongodb "$repetition" DW_MONGO_URL="$url"
    done
}

clickhouse() {
    start_container "dw-p3-clickhouse-$$" "$CLICKHOUSE_IMAGE" \
        -e CLICKHOUSE_SKIP_USER_SETUP=1 \
        -p 127.0.0.1::9000 -p 127.0.0.1::8123
    local id=$LAST_CONTAINER
    local port
    port=$(wait_clickhouse "$id")
    local dir=$CRATE
    [[ $VARIANT == patched ]] && dir=$(fixed_scenario_copy clickhouse)
    run_one clickhouse "$dir" clickhouse DW_CLICKHOUSE_ADDR="127.0.0.1:$port"
}

mkdir -p "$LOG" "$WORK/phase3"
echo "phase3 db-clients: variant=$VARIANT reps=$REPS targets=$TARGETS"
for target in $TARGETS; do
    case $target in
        mysql_async) mysql ;;
        mongodb) mongodb ;;
        clickhouse) clickhouse ;;
        *) echo "unknown target: $target" >&2; exit 2 ;;
    esac
done
