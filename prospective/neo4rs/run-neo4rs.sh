#!/usr/bin/env bash
# neo4rs 0.8.0 cancellation ladder (paired-scan follow-up).
#
# Starts one disposable Neo4j community container on 127.0.0.1 and stops it on
# exit. Every level of N1/N2 runs REPS times; the script prints the raw LADDER
# lines and saves them under prospective/neo4rs/logs/.
#
# Usage:
#   bash prospective/neo4rs/run-neo4rs.sh                 # release, 3 reps
#   VARIANT=patched bash prospective/neo4rs/run-neo4rs.sh # patch contrast
#   REPS=1 bash prospective/neo4rs/run-neo4rs.sh
#
# Environment: REPS (default 3), VARIANT=release|patched (default release),
# TIMEOUT (per test process, default 900), DROPWISE_NEO4RS_WORK (patched crate
# copies), DW_NEO4J_URL (use an existing server instead of starting a container).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CRATE=$ROOT/prospective/neo4rs
WORK=${DROPWISE_NEO4RS_WORK:-$HOME/dropwise-neo4rs-work}
REPS=${REPS:-3}
VARIANT=${VARIANT:-release}
TIMEOUT=${TIMEOUT:-900}
LOG=$CRATE/logs

# Pinned Neo4j community image. The digest recorded in RESULTS.md is the
# manifest-list digest this tag resolved to when the experiment was run.
NEO4J_IMAGE=neo4j:5.26.0-community
NEO4J_DIGEST=sha256:5a015e53de1895e7eee1574ae0325cf8c4b89587222778108c594bdd45a474b5

container=
cleanup() {
    if [[ -n ${container:-} ]]; then
        docker rm -f "$container" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

wait_neo4j() {
    local id=$1
    for ((attempt = 0; attempt < 240; attempt++)); do
        if docker exec "$id" cypher-shell -u neo4j -p dropwise --format plain 'RETURN 1' >/dev/null 2>&1; then
            docker port "$id" 7687/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}'
            return
        fi
        sleep 0.5
    done
    echo "Neo4j did not become ready" >&2
    exit 1
}

if [[ -z ${DW_NEO4J_URL:-} ]]; then
    docker image inspect "$NEO4J_IMAGE" >/dev/null 2>&1 || docker pull "$NEO4J_IMAGE"
    actual=$(docker image inspect "$NEO4J_IMAGE" --format '{{index .RepoDigests 0}}')
    echo "neo4j image: $NEO4J_IMAGE ($actual)"
    if [[ $actual != "neo4j@$NEO4J_DIGEST" ]]; then
        echo "warning: $NEO4J_IMAGE now resolves to $actual, expected neo4j@$NEO4J_DIGEST" >&2
    fi
    container=$(docker run --rm -d --name "dw-neo4rs-$$" \
        -p 127.0.0.1::7687 -e NEO4J_AUTH=neo4j/dropwise "$NEO4J_IMAGE")
    port=$(wait_neo4j "$container")
    export DW_NEO4J_URL="bolt://127.0.0.1:$port"
    echo "neo4j listening on $DW_NEO4J_URL"
fi

# A patched crate copy: copy the registry source, apply one diff, return path.
patched_source() { # crate-version, diff -> path
    local src
    src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$1" | head -n1)
    [[ -n $src ]] || { echo "$1 is not in the cargo registry cache" >&2; return 1; }
    local dest=$WORK/$1-fixed
    rm -rf "$dest"
    cp -r "$src" "$dest"
    patch -d "$dest" -p1 --quiet < "$CRATE/patches/neo4rs/$2"
    echo "$dest"
}

# A copy of the scenario crate pointing at the patched dependency.
fixed_scenario_copy() { # manifest dir
    local dest=$WORK/neo4rs-fixed
    rm -rf "$dest"
    mkdir -p "$dest"
    tar -C "$CRATE" --exclude=./target --exclude=./logs -cf - . | tar -C "$dest" -xf -
    sed -i "s#path = \"../..\"#path = \"$ROOT\"#" "$dest/Cargo.toml"
    printf '\n' >> "$dest/Cargo.toml"
    sed "s#@WORK@#$WORK#g" "$CRATE/patches/neo4rs/Cargo.patch.toml" >> "$dest/Cargo.toml"
    echo "$dest"
}

dir=$CRATE
if [[ $VARIANT == patched ]]; then
    mkdir -p "$WORK/support"
    cp "$ROOT/prospective/support/ladder.rs" "$WORK/support/"
    patched_source neo4rs-0.8.0 neo4rs-0.8.0-cancelled-exchange.diff >/dev/null
    dir=$(fixed_scenario_copy)
fi

mkdir -p "$LOG"
echo "neo4rs ladder: variant=$VARIANT reps=$REPS url=$DW_NEO4J_URL"
for repetition in $(seq "$REPS"); do
    out=$LOG/$VARIANT-$repetition.log
    printf '\n=== neo4rs %s, repetition %s/%s ===\n' "$VARIANT" "$repetition" "$REPS"
    if ! env SCAN_VARIANT="$VARIANT" DW_DIAG=1 timeout "${TIMEOUT}s" \
        cargo test --offline --manifest-path "$dir/Cargo.toml" \
        --test neo4rs -- --nocapture --test-threads=1 2>&1 | tee "$out"; then
        echo "=== neo4rs $VARIANT repetition $repetition failed (see $out)" >&2
    fi
done
