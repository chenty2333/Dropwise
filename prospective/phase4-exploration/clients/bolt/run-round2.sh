#!/usr/bin/env bash
# Round 2 only. The first-round Cargo.toml, Cargo.lock, tests/ and logs/ are
# not modified. This script uses two separate projects so the registry bb8
# 0.8.6 baseline stays reproducible:
#   round2/original  registry bb8 0.8.6
#   round2/fixed     path patch: vendor/bb8-0.8.6 with the
#                    invalid-until-validated change in Pool::get
# MODE=generate runs the registered target/successor cases only.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="dw-p4e-bolt-r2-$$"
IMAGE="neo4j:5.26.0-community"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"
MODE=${MODE:-round2}

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

docker run -d --rm --name "$NAME" \
  -p 127.0.0.1::7687 \
  -e NEO4J_AUTH=neo4j/dropwise \
  "$IMAGE" >/dev/null

for _ in $(seq 1 240); do
  if docker exec "$NAME" cypher-shell -u neo4j -p dropwise --format plain 'RETURN 1' >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
if ! docker exec "$NAME" cypher-shell -u neo4j -p dropwise --format plain 'RETURN 1' >/dev/null 2>&1; then
  echo "Neo4j did not become ready" >&2
  exit 1
fi

PORT="$(docker port "$NAME" 7687/tcp | awk -F: '{print $NF}')"
export DW_NEO4J_URL="bolt://127.0.0.1:$PORT"
export DW_DIAG=1

{
  date -u +"date_utc=%Y-%m-%dT%H:%M:%SZ"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "image=$IMAGE"
  echo "image_digest=$(docker image inspect "$IMAGE" --format '{{index .RepoDigests 0}}')"
  echo "url=$DW_NEO4J_URL"
  echo "container=$NAME"
  echo "neo4j=$(docker exec "$NAME" cypher-shell -u neo4j -p dropwise --format plain 'CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition' | tail -1)"
  echo "--- round2 original resolved versions ---"
  awk '/^name = "(bolt-client|bb8-bolt|bb8|bolt-proto|dropwise)"$/{name=$0; getline; print name " " $0}' "$HERE/round2/original/Cargo.lock"
  echo "--- round2 fixed resolved versions ---"
  awk '/^name = "(bolt-client|bb8-bolt|bb8|bolt-proto|dropwise)"$/{name=$0; getline; print name " " $0}' "$HERE/round2/fixed/Cargo.lock"
} > "$LOGS/round2-environment.txt"

(
  cd "$HERE/round2/original"
  cargo tree --offline -e normal --depth 2
) > "$LOGS/round2-deps-original.txt" 2>&1 || true
(
  cd "$HERE/round2/fixed"
  cargo tree --offline -e normal --depth 2
) > "$LOGS/round2-deps-fixed.txt" 2>&1 || true

(
  cd "$HERE/round2/original"
  cargo test --offline --no-run
) > "$LOGS/round2-build-original.log" 2>&1
(
  cd "$HERE/round2/fixed"
  cargo test --offline --no-run
) > "$LOGS/round2-build-fixed.log" 2>&1

if [[ $MODE == generate ]]; then
  status=0
  for variant in original fixed; do
    log="$LOGS/generate-$variant.log"
    : > "$log"
    echo "=== generated cases, variant=$variant ===" >> "$log"
    echo "command: cargo test --offline --test generate -- --nocapture --test-threads=1" >> "$log"
    if ! ( cd "$HERE/round2/$variant" && \
      cargo test --offline --test generate -- --nocapture --test-threads=1 ) >> "$log" 2>&1; then
      echo "FAILED: variant=$variant test=generate" | tee -a "$log" >&2
      status=1
    fi
  done
  exit "$status"
fi

LEVELS=(normal_checkout is_valid_failure checkout_only pooled_b2 pooled_b3 pooled_m)

run_level() {
  local variant="$1"
  local level="$2"
  local log="$3"
  echo "===== variant=$variant level=$level =====" >> "$log"
  echo "command: cargo test --offline --test round2 -- --exact $level --nocapture --test-threads=1" >> "$log"
  ( cd "$HERE/round2/$variant" && \
    cargo test --offline --test round2 -- --exact "$level" --nocapture --test-threads=1 ) >> "$log" 2>&1
}

status=0
for rep in 1 2 3; do
  for variant in original fixed; do
    log="$LOGS/round2-$variant-rep$rep.log"
    : > "$log"
    echo "=== round2 $variant repetition $rep ===" >> "$log"
    for level in "${LEVELS[@]}"; do
      if ! run_level "$variant" "$level" "$log"; then
        echo "FAILED: variant=$variant rep=$rep level=$level" | tee -a "$log" >&2
        status=1
      fi
    done
  done
done

# The replay test explores the original checkout scenario, takes the first
# actual violation, calls Trial::replay_plan, and attempts replay_observed.
# It prints the source plan/site/value and each replay attempt.
if ! ( cd "$HERE/round2/original" && \
  cargo test --offline --test round2 -- --exact replay_checkout --nocapture --test-threads=1 ) \
  > "$LOGS/round2-replay-original.log" 2>&1; then
  echo "FAILED: replay_checkout" >&2
  status=1
fi

exit "$status"
