#!/usr/bin/env bash
# Round-3 skytable runner. One disposable skytable v0.8.4 container on a
# loopback-only ephemeral port, both reuse modes, new logs/run.XXXXXX only.
set -eo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
cd "$HERE"
new_logdir "$HERE"
CID=""
NAME="dw-r3-sky-$$"

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

echo "== build =="
CARGO_BUILD_JOBS=6 cargo build --release --offline > "$LOGS/build.log" 2>&1 || {
  CARGO_BUILD_JOBS=6 cargo build --release > "$LOGS/build.log" 2>&1
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
# The image prints the password before the listener reliably accepts a
# handshake. Wait for a real authenticated sysctl, not just an open socket.
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
  echo "server_password_source=docker logs 'Generated Password'"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms"
  echo "config=max_runs=64 races=Immediate,AfterWake max_cancellations=1 scenario_timeout=20s settle=0/0 workers=1"
} > "$LOGS/environment.txt"

: > "$LOGS/timings.txt"
STATUS=0
run_phase() {
  local name="$1"; shift
  local start; start="$(date +%s)"
  set +e
  "$@" > "$LOGS/$name.stdout.log" 2> "$LOGS/$name.stderr.log"
  local rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  set -e
  echo "$name rc=$rc seconds=$(( $(date +%s) - start ))" >> "$LOGS/timings.txt"
  echo "$name rc=$rc" >&2
}

BIN="$HERE/target/release/sky-ladder"
export SCAN_VARIANT=skytable
for variant in pooled direct; do
  run_phase "preflight-$variant" "$BIN" preflight "$variant"
  for rep in 1 2 3; do
    run_phase "ladder-$variant-$rep" "$BIN" ladder "$variant" "$rep"
  done
  run_phase "recovery-$variant" "$BIN" recovery "$variant"
done

if [ -x "$HERE/target/release/sky-generate" ]; then
  for rep in 1 2 3; do
    run_phase "generate-$rep" "$HERE/target/release/sky-generate" "$rep"
  done
fi

echo "done logs=$LOGS"

exit "$STATUS"
