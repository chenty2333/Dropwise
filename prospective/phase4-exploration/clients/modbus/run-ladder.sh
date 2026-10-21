#!/usr/bin/env bash
# Round-3 tokio-modbus runner. One standalone Python asyncio Modbus TCP server
# on a loopback ephemeral port; no container. New logs/run.XXXXXX only.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"

SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== build =="
cargo build --release --offline > "$LOGS/build.log" 2>&1 || cargo build --release >> "$LOGS/build.log" 2>&1

echo "== server =="
python3 "$HERE/modbus_server.py" --port 0 --delay-ms 0 --log "$LOGS/server-requests.jsonl" \
  > "$LOGS/server.out" 2>&1 &
SERVER_PID=$!
PORT=""
for _ in $(seq 1 100); do
  PORT="$(sed -n 's/^PORT=//p' "$LOGS/server.out" | head -1)"
  [ -n "$PORT" ] && break
  sleep 0.1
done
if [ -z "$PORT" ]; then
  echo "python modbus server did not print a port" >&2
  cat "$LOGS/server.out" >&2
  exit 1
fi
export DW_MODBUS_SERVER="127.0.0.1:$PORT"
echo "server=127.0.0.1:$PORT"

{
  echo "date=$(date -Is)"
  echo "repo=$ROOT"
  echo "server=standalone python3 asyncio Modbus TCP (public MBAP), pid=$SERVER_PID, 127.0.0.1:$PORT"
  echo "server_delay_ms=0"
  echo "client=tokio-modbus 0.17.0 (direct client::Context)"
  echo "tokio=1.54.0 dropwise=path:$ROOT"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms"
  echo "config=max_runs=64 races=Immediate,AfterWake max_cancellations=1 scenario_timeout=20s settle=0/0 workers=1"
  echo "python=$(python3 -V 2>&1)"
  cargo --version
  rustc --version
} > "$LOGS/environment.txt"

: > "$LOGS/timings.txt"
run_phase() {
  local name="$1"; shift
  local start; start="$(date +%s)"
  set +e
  "$@" > "$LOGS/$name.stdout.log" 2> "$LOGS/$name.stderr.log"
  local rc=$?
  set -e
  echo "$name rc=$rc seconds=$(( $(date +%s) - start ))" >> "$LOGS/timings.txt"
  echo "$name rc=$rc" >&2
  return 0
}

BIN="$HERE/target/release/modbus-ladder"
export SCAN_VARIANT=modbus
run_phase preflight "$BIN" preflight
for side in pre post; do
  for rep in 1 2; do
    run_phase "calibrate-$side-$rep" "$BIN" "calibrate-$side"
  done
done
for rep in 1 2 3; do
  run_phase "ladder-$rep" "$BIN" ladder "$rep"
done

cp "$LOGS/server-requests.jsonl" "$LOGS/server-requests.snapshot.jsonl" 2>/dev/null || true
echo "done logs=$LOGS"
