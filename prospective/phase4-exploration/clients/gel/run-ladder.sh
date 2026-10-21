#!/usr/bin/env bash
# Round-3 Gel ladder runner. One disposable Gel 7.1 container on a
# loopback-only ephemeral port, insecure dev mode, removed on exit.
# Writes only a new logs/run.XXXXXX directory.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"

CID=""
NAME="dw-gel-ladder-$$"
cleanup() {
  if [ -n "$CID" ]; then
    docker logs "$CID" > "$LOGS/container.log" 2>&1 || true
    docker rm -f "$CID" >/dev/null 2>&1 || true
    CID=""
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== build =="
set +e
cargo build --offline > "$LOGS/build.log" 2>&1
RC=$?
if [ "$RC" -ne 0 ]; then
  cargo build >> "$LOGS/build.log" 2>&1
  RC=$?
fi
set -e
if [ "$RC" -ne 0 ]; then
  echo "build failed, see $LOGS/build.log" >&2
  exit 1
fi
BIN="$HERE/target/debug/p4e-gel"
[ -x "$BIN" ] || { echo "missing $BIN" >&2; exit 1; }

echo "== server =="
IMAGE="geldata/gel:7"
CID="$(docker run -d --rm --name "$NAME" -e GEL_SERVER_SECURITY=insecure_dev_mode \
  -p 127.0.0.1::5656 "$IMAGE")"
PORT=""
for _ in $(seq 1 180); do
  PORT="$(docker port "$CID" 5656/tcp 2>/dev/null | awk -F: '{print $NF}')"
  if [ -n "$PORT" ] && docker exec "$CID" gel query --host 127.0.0.1 --port 5656 \
       --user admin --tls-security insecure 'select 1' >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
if [ -z "$PORT" ] || ! docker exec "$CID" gel query --host 127.0.0.1 --port 5656 \
     --user admin --tls-security insecure 'select 1' >/dev/null 2>&1; then
  echo "Gel did not become ready" >&2
  docker logs "$CID" >&2 || true
  exit 1
fi
export DW_GEL_SERVER="127.0.0.1:$PORT"
echo "server=127.0.0.1:$PORT"

SERVER_VERSION="$(docker exec "$CID" gel query --host 127.0.0.1 --port 5656 \
  --user admin --tls-security insecure 'select sys::get_version_as_str()' 2>/dev/null | tr -d '\r' || true)"
{
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "repo=$ROOT"
  echo "git_head=$(git -C "$ROOT" rev-parse HEAD)"
  echo "container=$NAME"
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}')"
  echo "repo_digest=$(docker image inspect "$IMAGE" --format '{{join .RepoDigests ","}}')"
  echo "server_version_log=$(docker logs "$CID" 2>&1 | grep -o 'starting Gel server [^ ]*' | head -1)"
  echo "server_version_query=$SERVER_VERSION"
  echo "port=127.0.0.1:$PORT"
  echo "bootstrap=GEL_SERVER_SECURITY=insecure_dev_mode"
  echo "auth=user admin, branch main, TlsSecurity::Insecure"
  echo "client=gel-tokio 0.11.0 (Client pool; max_concurrency(1) => at most one connection at a time, not a persistent direct handle)"
  echo "client_crates:"
  cargo tree -p gel-tokio --depth 1 2>/dev/null | sed 's/^/  /' | head -40
  echo "lock_pins:"
  grep -A1 -E '^name = "(gel-tokio|gel-stream|derive_more|tokio|dropwise)"$' Cargo.lock | \
    grep -E 'name|version' | paste - - | sed 's/^/  /'
  echo "build_profile=debug"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms"
  echo "config=max_runs=64 races=Immediate,AfterWake max_cancellations=1 scenario_timeout=20s settle=tokio0/real0/watchdog10s workers=1"
  echo "target=Client::transaction(update DwGel v=1,marker=target select (v,marker))"
  echo "survivor=Client::transaction(update DwGel v=7,marker=survivor)"
  echo "observer=independent Client direct to server"
  cargo --version
  rustc --version
} > "$LOGS/environment.txt"

: > "$LOGS/timings.txt"
run_phase() {
  local name="$1"; shift
  local start; start="$(date +%s.%N)"
  set +e
  "$@" > "$LOGS/$name.stdout.log" 2> "$LOGS/$name.stderr.log"
  local rc=$?
  set -e
  local end; end="$(date +%s.%N)"
  awk -v n="$name" -v rc="$rc" -v s="$start" -v e="$end" \
    'BEGIN { printf "%s rc=%d seconds=%.3f\n", n, rc, e-s }' >> "$LOGS/timings.txt"
  echo "$name rc=$rc" >&2
  return 0
}

export SCAN_VARIANT=gel
run_phase preflight "$BIN" preflight
if ! grep -q 'GEL-PREFLIGHT-OK' "$LOGS/preflight.stdout.log"; then
  echo "preflight failed, see $LOGS/preflight.stdout.log" >&2
  exit 1
fi
for rep in 1 2 3; do
  run_phase "calibrate-pre-rep$rep" "$BIN" calibrate-pre "$rep"
  run_phase "calibrate-post-rep$rep" "$BIN" calibrate-post "$rep"
  for level in B1 B2 B3 M1; do
    run_phase "ladder-$level-rep$rep" "$BIN" ladder "$rep" "$level"
  done
done

# Digest of the ladder lines and trial lines, for the results fragment.
{
  for rep in 1 2 3; do
    for level in B1 B2 B3 M1; do
      grep -E '^GEL-LADDER|^GEL-VIOLATION|^GEL-TRIAL' \
        "$LOGS/ladder-$level-rep$rep.stdout.log" || true
    done
  done
} > "$LOGS/counts.txt"

echo "done logs=$LOGS"
