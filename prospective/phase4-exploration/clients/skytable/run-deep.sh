#!/usr/bin/env bash
# Deep pass for the round-3 skytable candidate. One disposable skytable v0.8.4
# container on loopback, a new logs/run.XXXXXX only; old logs are untouched.
#
# Phases:
#   1. sky-deep: request/reply trace (direct read-cut, pooled checkout-cut and
#      pooled update-cut, plus a logging-proxy wire run).
#   2. original p4e-skytable ladder, 3 reps pooled/direct (+preflight).
#   3. fixed p4e-skytable (dependency-copy patch), same ladder + preflight +
#      recovery contrast.
set -eo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"
CID=""
NAME="dw-r3-skydeep-$$"

cleanup() {
  if [ -n "$CID" ]; then
    docker logs "$CID" > "$LOGS/container.log" 2>&1 || true
    docker stop "$CID" >/dev/null 2>&1 || true
    CID=""
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== build original =="
CARGO_BUILD_JOBS=6 cargo build --release --offline --manifest-path "$HERE/Cargo.toml" > "$LOGS/build-original.log" 2>&1 || {
  CARGO_BUILD_JOBS=6 cargo build --release --manifest-path "$HERE/Cargo.toml" > "$LOGS/build-original.log" 2>&1
}
echo "== build fixed (dependency copy) =="
FIXED_DIR="$HERE/fix/p4e-skytable-fixed"
CARGO_BUILD_JOBS=6 cargo build --release --offline --manifest-path "$FIXED_DIR/Cargo.toml" > "$LOGS/build-fixed.log" 2>&1 || {
  CARGO_BUILD_JOBS=6 cargo build --release --manifest-path "$FIXED_DIR/Cargo.toml" > "$LOGS/build-fixed.log" 2>&1
}

CID="$(docker run -d --rm --name "$NAME" -p 127.0.0.1::2003 skytable/skytable:v0.8.4)"
PASS=""
for _ in $(seq 1 60); do
  PASS="$(docker logs "$CID" 2>&1 | sed -n 's/Generated Password: //p' | head -1)"
  [ -n "$PASS" ] && break
  sleep 0.5
done
PORT="$(docker port "$CID" 2003/tcp | head -1 | sed 's/.*://')"
if [ -z "$PASS" ] || [ -z "$PORT" ]; then
  echo "skytable did not print its generated password / port" >&2
  exit 1
fi
export DW_SKY_SERVER="127.0.0.1:$PORT"
export DW_SKY_USER=root
export DW_SKY_PASS="$PASS"

READY=0
for _ in $(seq 1 60); do
  if "$HERE/target/release/sky-ladder" wait 2>/dev/null; then
    READY=1
    break
  fi
  sleep 0.5
done
if [ "$READY" -ne 1 ]; then
  echo "skytable never accepted an authenticated handshake" >&2
  exit 1
fi

{
  echo "date=$(date -Is)"
  echo "repo=$ROOT"
  echo "container=$NAME"
  echo "image=skytable/skytable:v0.8.4"
  echo "image_id=$(docker image inspect skytable/skytable:v0.8.4 --format '{{.Id}}')"
  echo "repo_digest=$(docker image inspect skytable/skytable:v0.8.4 --format '{{join .RepoDigests ","}}')"
  echo "port=127.0.0.1:$PORT"
  echo "skytable_client=0.8.12 bb8=0.9.1"
  echo "original_bin=sha256:$(sha256sum "$HERE/target/release/sky-ladder" | cut -d' ' -f1)"
  echo "deep_bin=sha256:$(sha256sum "$HERE/target/release/sky-deep" | cut -d' ' -f1)"
  echo "fixed_bin=sha256:$(sha256sum "$FIXED_DIR/target/release/sky-ladder" | cut -d' ' -f1)"
  echo "fixed_skytable_sha=sha256:$(sha256sum "$HERE/fix/skytable-0.8.12/src/io/aio.rs" "$HERE/fix/skytable-0.8.12/src/pool.rs" | sha256sum | cut -d' ' -f1)"
  echo "patch_files=fix/skytable-0.8.12/src/io/aio.rs,fix/skytable-0.8.12/src/pool.rs"
  echo "server_password_source=docker logs 'Generated Password'"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms (scenario); logging proxy 50ms (wire)"
  echo "config=max_runs=64 races=Immediate,AfterWake max_cancellations=1 scenario_timeout=20s settle=0/0 workers=1"
  echo "env_log=RUST_LOG=debug for deep/wire (skytable/bb8/dropwise emit no log records; no request id in protocol)"
} > "$LOGS/environment.txt"

: > "$LOGS/timings.txt"
STATUS=0
run_phase() {
  local name="$1"; shift
  local start; start="$(date +%s)"
  set +e
  RUST_LOG=debug "$@" > "$LOGS/$name.stdout.log" 2> "$LOGS/$name.stderr.log"
  local rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  set -e
  echo "$name rc=$rc seconds=$(( $(date +%s) - start ))" >> "$LOGS/timings.txt"
  echo "$name rc=$rc" >&2
}

DEEP="$HERE/target/release/sky-deep"
ORIG="$HERE/target/release/sky-ladder"
FIXED="$FIXED_DIR/target/release/sky-ladder"

# 1. Request/reply trace. `wire` uses its own logging forwarder; the others use
# the same dropwise proxy as the scenario.
run_phase deep-wire-direct-cut10 "$DEEP" wire 10 50 1
for rep in 1 2 3; do
  run_phase "deep-direct-cut10-rep$rep" "$DEEP" direct 10 50 "$rep"
done
run_phase deep-direct-cut1 "$DEEP" direct 1 50 4
for rep in 1 2 3; do
  run_phase "deep-pooled-cut10-rep$rep" "$DEEP" pooled 10 50 "$rep"
done
run_phase deep-pooled-cut70 "$DEEP" pooled 70 50 6
run_phase deep-pooled-cut120 "$DEEP" pooled 120 50 4
run_phase deep-pooled-cut120-d200 "$DEEP" pooled 120 200 5

# 2. Original ladder, same scenario/config as run.VlevMa plus residue prints.
export SCAN_VARIANT=skytable-0.8.12
for variant in pooled direct; do
  run_phase "orig-preflight-$variant" "$ORIG" preflight "$variant"
  for rep in 1 2 3; do
    run_phase "orig-ladder-$variant-$rep" "$ORIG" ladder "$variant" "$rep"
  done
done

# 3. Fixed ladder: same source, patched dependency only.
export SCAN_VARIANT=skytable-fixed
for variant in pooled direct; do
  run_phase "fixed-preflight-$variant" "$FIXED" preflight "$variant"
  for rep in 1 2 3; do
    run_phase "fixed-ladder-$variant-$rep" "$FIXED" ladder "$variant" "$rep"
  done
  run_phase "fixed-recovery-$variant" "$FIXED" recovery "$variant"
done

echo "done logs=$LOGS"

exit "$STATUS"
