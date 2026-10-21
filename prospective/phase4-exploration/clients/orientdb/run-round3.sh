#!/usr/bin/env bash
# Round 3 OrientDB runner. One disposable orientdb:3.0.23 container, one unique
# plocal database created through the documented HTTP API, loopback-only.
# Produces logs/run.XXXXXX/ and never overwrites previous runs.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="dw-r3-ori-$$"
IMAGE="orientdb:3.0.23"
DB="dw_r3_$$_$(date -u +%H%M%S)"
ROOT_PASS="dwroot"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"
START=$(date +%s)

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

docker run -d --rm --name "$NAME" \
  -e ORIENTDB_ROOT_PASSWORD="$ROOT_PASS" \
  -p 127.0.0.1::2424 -p 127.0.0.1::2480 \
  "$IMAGE" >/dev/null

HP=""
for _ in $(seq 1 120); do
  HP="$(docker port "$NAME" 2480/tcp 2>/dev/null | awk -F: '{print $NF}')"
  if [[ -n "$HP" ]] && curl -sf -u "root:$ROOT_PASS" "http://127.0.0.1:$HP/listDatabases" >/dev/null; then
    break
  fi
  sleep 0.5
done
if [[ -z "$HP" ]] || ! curl -sf -u "root:$ROOT_PASS" "http://127.0.0.1:$HP/listDatabases" >/dev/null; then
  echo "OrientDB HTTP endpoint did not become ready" >&2
  docker logs "$NAME" >&2 || true
  exit 1
fi
BP="$(docker port "$NAME" 2424/tcp | awk -F: '{print $NF}')"

CREATE_CODE="$(curl -s -o "$LOGS/create-database.txt" -w '%{http_code}' \
  -u "root:$ROOT_PASS" -X POST "http://127.0.0.1:$HP/database/$DB/plocal")"
echo "create_database http=$CREATE_CODE db=$DB" | tee "$LOGS/create-database.status"
[[ "$CREATE_CODE" == "200" ]]

export DW_ORI_HOST=127.0.0.1
export DW_ORI_PORT="$BP"
export DW_ORI_DB="$DB"
export DW_ORI_USER=root
export DW_ORI_PASS="$ROOT_PASS"
export CARGO_BUILD_JOBS=6

{
  date -u +"date_utc=%Y-%m-%dT%H:%M:%SZ"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}')"
  echo "image_digest=$(docker image inspect "$IMAGE" --format '{{index .RepoDigests 0}}')"
  echo "container=$NAME"
  echo "http_port=$HP"
  echo "binary_port=$BP"
  echo "db=$DB"
  echo "orientdb=$(
    curl -s -u "root:$ROOT_PASS" "http://127.0.0.1:$HP/listDatabases"
  )"
  echo "--- resolved versions ---"
  awk '/^name = "(orientdb-client|mobc|dropwise)"$/{name=$0; getline; print name " " $0}' "$HERE/Cargo.lock"
} > "$LOGS/environment.txt"

(
  cd "$HERE"
  cargo test --offline --test orientdb --no-run
  cargo test --offline --test session --no-run
  cargo test --offline --test generate --no-run
) > "$LOGS/build.log" 2>&1

run_level() {
  local binary="$1" test="$2" tag="$3"
  local log="$LOGS/level-${tag}.log"
  echo "===== binary=$binary test=$test tag=$tag =====" >> "$log"
  echo "command: cargo test --offline --test $binary -- --exact $test --nocapture --test-threads=1" >> "$log"
  ( cd "$HERE" && cargo test --offline --test "$binary" -- --exact "$test" --nocapture --test-threads=1 ) >> "$log" 2>&1
}

STATUS=0
for rep in 1 2 3; do
  for level in "orientdb b1_pooled B1" "orientdb b2_pooled B2" "orientdb b3_pooled B3" "orientdb m1_pooled M1" \
               "session b1_session S-B1" "session b2_session S-B2" "session b3_session S-B3" "session m1_session S-M1"; do
    set -- $level
    if ! run_level "$1" "$2" "${3}-rep${rep}"; then
      echo "FAILED: rep=$rep level=${3}" >&2
      STATUS=1
    fi
  done
done

# One-time non-cancelled diagnostics.
for spec in "preflight" "reuse" "recovery_contrast"; do
  log="$LOGS/diagnostic-$spec.log"
  echo "===== diagnostic=$spec =====" >> "$log"
  ( cd "$HERE" && cargo test --offline --test orientdb -- --exact "$spec" --nocapture --test-threads=1 ) >> "$log" 2>&1 || STATUS=1
done

# Generated target x successor cases, one run, separate binary/prefix.
log="$LOGS/generate.log"
echo "===== generated cases =====" >> "$log"
( cd "$HERE" && cargo test --offline --test generate -- --exact generated_orientdb_cases --nocapture --test-threads=1 ) >> "$log" 2>&1 || STATUS=1

END=$(date +%s)
{
  echo "wall_seconds=$((END - START))"
  echo "levels=3 reps x (B1 B2 B3 M1 + S-B1 S-B2 S-B3 S-M1) = 24 level runs"
  echo "diagnostics=preflight,reuse,recovery_contrast"
  echo "generated_cases=13 (one run)"
  echo "status=$STATUS"
} > "$LOGS/cost.txt"
echo "logs=$LOGS status=$STATUS"
exit "$STATUS"
