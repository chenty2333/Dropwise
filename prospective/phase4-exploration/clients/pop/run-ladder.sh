#!/usr/bin/env bash
# Round-3 async-pop runner. GreenMail 2.1.14 is present and was probed, but
# async-pop 1.1.3 `login` hangs on GreenMail's chunked CAPA reply (client
# decode_needs is not reset), so the allowed fallback is used: a loopback
# Python POP3 server with real protocol and a server-owned HTTP observer. New
# logs/run.XXXXXX only.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
cd "$HERE"
new_logdir "$HERE"
SERVER_PID=""
USER_EMAIL="popuser@example.com"
PASS="pass"
SUBJECT="dw-r3-pop-seed"

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

echo "== pop server =="
python3 "$HERE/pop_server.py" --pop-port 0 --obs-port 0 --log "$LOGS/pop-commands.jsonl" \
  > "$LOGS/server.out" 2>&1 &
SERVER_PID=$!
POP_PORT=""
OBS_PORT=""
for _ in $(seq 1 100); do
  line="$(sed -n 's/^POP_PORT=//p' "$LOGS/server.out" | head -1)"
  if [ -n "$line" ]; then
    POP_PORT="$(echo "$line" | awk '{print $1}')"
    OBS_PORT="$(echo "$line" | sed -n 's/.*OBS_PORT=//p')"
    break
  fi
  sleep 0.1
done
if [ -z "$POP_PORT" ] || [ -z "$OBS_PORT" ]; then
  echo "python POP3 server did not print ports" >&2
  cat "$LOGS/server.out" >&2
  exit 1
fi

# Wait for a real POP3 greeting, not just an open socket.
READY=0
for _ in $(seq 1 100); do
  if python3 - "$POP_PORT" <<'PY' 2>/dev/null
import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1)
banner = s.recv(200)
s.close()
sys.exit(0 if banner.startswith(b"+OK") else 1)
PY
  then READY=1; break; fi
  sleep 0.1
done
if [ "$READY" -ne 1 ]; then
  echo "python POP3 server never greeted" >&2
  exit 1
fi

# The server owns one seeded message; verify through its HTTP observer.
COUNT="$(curl -s "http://127.0.0.1:$OBS_PORT/state" | sed -n 's/.*"messages": \([0-9]*\).*/\1/p')"
if [ "$COUNT" != "1" ]; then
  echo "observer does not show exactly one seeded message: $COUNT" >&2
  exit 1
fi

export DW_POP_ADDR="127.0.0.1:$POP_PORT"
export DW_POP_OBS="127.0.0.1:$OBS_PORT"
export DW_POP_USER="$USER_EMAIL"
export DW_POP_PASS="$PASS"

{
  echo "date=$(date -Is)"
  echo "repo=$ROOT"
  echo "server=standalone python3 asyncio POP3 (real protocol, one seeded message), pid=$SERVER_PID"
  echo "server_ports=127.0.0.1:$POP_PORT(pop3) 127.0.0.1:$OBS_PORT(observer-http)"
  echo "seed=server-owned single message, subject=$SUBJECT, size=bytes, observer_count=$COUNT"
  echo "greenmail_precheck=image greenmail/standalone:2.1.14 id=$(docker image inspect greenmail/standalone:2.1.14 --format '{{.Id}}' 2>/dev/null || echo unavailable)"
  echo "greenmail_fallback_reason=async-pop 1.1.3 login hangs on GreenMail CAPA reply chunking (decode_needs not reset); recorded in RESULTS-FRAGMENT.md"
  echo "client=async-pop 1.1.3 (direct async_pop::Client<tokio TcpStream>), default-features=false runtime-tokio"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms"
  echo "config=max_runs=64 races=Immediate,AfterWake max_cancellations=1 scenario_timeout=20s settle=0/0 workers=1"
  echo "python=$(python3 -V 2>&1)"
  cargo --version
  rustc --version
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
  return 0
}

BIN="$HERE/target/release/pop-ladder"
export SCAN_VARIANT=pop
run_phase preflight "$BIN" preflight
for side in pre post; do
  for rep in 1 2; do
    run_phase "calibrate-$side-$rep" "$BIN" "calibrate-$side"
  done
done
for rep in 1 2 3; do
  run_phase "ladder-$rep" "$BIN" ladder "$rep"
done

echo "done logs=$LOGS"

exit "$STATUS"
