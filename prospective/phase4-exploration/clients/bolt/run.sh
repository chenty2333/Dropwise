#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="dw-p4e-bolt-$$"
IMAGE="neo4j:5.26.0-community"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"

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
  echo "neo4j=$(docker exec "$NAME" cypher-shell -u neo4j -p dropwise --format plain 'CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition' | tail -1)"
  echo "--- resolved versions of direct interest ---"
  awk '/^name = "(bolt-client|bb8-bolt|bb8|bolt-proto|dropwise)"$/{name=$0; getline; print name " " $0}' "$HERE/Cargo.lock"
} > "$HERE/environment.txt"

cd "$HERE"
cargo test --test bolt preflight -- --nocapture --test-threads=1 2>&1 | tee "logs/preflight.log"
cargo test --test bolt reuse -- --nocapture --test-threads=1 2>&1 | tee "logs/reuse.log"

for rep in 1 2 3; do
  echo "=== ladder repetition $rep ==="
  : > "logs/ladder-$rep.log"
  for t in pooled_b1 pooled_b2 pooled_b3 pooled_m direct_b1 direct_b2 direct_b3 direct_m; do
    cargo test --test bolt -- --exact "$t" --nocapture 2>&1 | tee -a "logs/ladder-$rep.log"
  done
done

for rep in 1 2 3; do
  echo "=== added explorations repetition $rep ==="
  : > "logs/added-$rep.log"
  for t in direct_m_reset_test direct_m_reset_delayed_test pooled_m_checkout_test pooled_m_checkout_nocheck_test; do
    cargo test --test bolt -- --exact "$t" --nocapture 2>&1 | tee -a "logs/added-$rep.log"
  done
done

rm -f "$HERE/Cargo.lock.bak"
