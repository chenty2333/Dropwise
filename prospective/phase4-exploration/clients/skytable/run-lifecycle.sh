#!/usr/bin/env bash
# Live lifecycle test for the cancelled-query/pipeline patch.
#
# One disposable skytable server with TCP and TLS endpoints, a fresh
# logs/run.XXXXXX, and identical probe binaries built against the stock
# crates.io client and the dependency-copy patch.  The probe only uses the
# public client API; it cancels a pipeline after it has been polled and then
# checks whether the next user gets a fresh reply, the cancelled reply, or an
# explicit ConnectionAborted.
#
# Modes: direct, pool2 (max_size=2, test_on_check_out=false, one connection
# held), tls-direct, tls-pool (max_size=1, test_on_check_out=false).
# The stock client is the control and must reproduce the stale reply; the
# fixed client must never return a stale reply.
set -eo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"
CID=""
CERTDIR=""
NAME="dw-r3-skylife-$$"
PASS="dw-lifecycle-pass-0123"

cleanup() {
  if [ -n "$CID" ]; then
    docker logs "$CID" > "$LOGS/container.log" 2>&1 || true
    docker stop "$CID" >/dev/null 2>&1 || true
    CID=""
  fi
  if [ -n "$CERTDIR" ] && [ -d "$CERTDIR" ]; then
    rm -rf -- "$CERTDIR"
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== build stock =="
CARGO_BUILD_JOBS=6 cargo build --release --offline --manifest-path "$HERE/Cargo.toml" --bin sky-lifecycle > "$LOGS/build-stock.log" 2>&1 || {
  CARGO_BUILD_JOBS=6 cargo build --release --manifest-path "$HERE/Cargo.toml" --bin sky-lifecycle > "$LOGS/build-stock.log" 2>&1
}
echo "== build fixed =="
CARGO_BUILD_JOBS=6 cargo build --release --offline --manifest-path "$HERE/fix/p4e-skytable-fixed/Cargo.toml" --bin sky-lifecycle > "$LOGS/build-fixed.log" 2>&1 || {
  CARGO_BUILD_JOBS=6 cargo build --release --manifest-path "$HERE/fix/p4e-skytable-fixed/Cargo.toml" --bin sky-lifecycle > "$LOGS/build-fixed.log" 2>&1
}

CERTDIR="$(mktemp -d /tmp/dw-sky-life.XXXXXX)"
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$CERTDIR/key.pem" -out "$CERTDIR/cert.pem" \
  -days 1 -subj "/CN=localhost" >/dev/null 2>&1
printf 'lifecycle\n' > "$CERTDIR/pass.txt"
chmod 644 "$CERTDIR"/*

echo "== server =="
CID="$(docker run -d --rm --name "$NAME" \
  -v "$CERTDIR:/certs:ro,z" \
  -p 127.0.0.1::2003 -p 127.0.0.1::2004 \
  --entrypoint skyd skytable/skytable:v0.8.4 \
  --mode dev --auth-root-password "$PASS" \
  --endpoint tcp@0.0.0.0:2003 --endpoint tls@0.0.0.0:2004 \
  --tlscert /certs/cert.pem --tlskey /certs/key.pem \
  --tls-passphrase /certs/pass.txt)"
READY=0
for _ in $(seq 1 80); do
  if docker logs "$CID" 2>&1 | grep -q "listening on"; then READY=1; break; fi
  sleep 0.25
done
if [ "$READY" -ne 1 ]; then
  echo "server did not start" >&2
  docker logs "$CID" 2>&1 | tail -20 >&2
  exit 1
fi
TCP_PORT="$(docker port "$CID" 2003/tcp | head -1 | sed 's/.*://')"
TLS_PORT="$(docker port "$CID" 2004/tcp | head -1 | sed 's/.*://')"
if [ -z "$TCP_PORT" ] || [ -z "$TLS_PORT" ]; then
  echo "missing published port" >&2
  exit 1
fi

export DW_SKY_SERVER="127.0.0.1:$TCP_PORT"
export DW_SKY_USER=root
export DW_SKY_PASS="$PASS"
export DW_SKY_TLS_SERVER="127.0.0.1:$TLS_PORT"
export DW_SKY_TLS_CERT="$CERTDIR/cert.pem"

{
  echo "date=$(date -Is)"
  echo "repo=$ROOT"
  echo "container=$NAME"
  echo "image=skytable/skytable:v0.8.4"
  echo "image_id=$(docker image inspect skytable/skytable:v0.8.4 --format '{{.Id}}')"
  echo "repo_digest=$(docker image inspect skytable/skytable:v0.8.4 --format '{{join .RepoDigests ","}}')"
  echo "tcp=127.0.0.1:$TCP_PORT tls=127.0.0.1:$TLS_PORT"
  echo "skytable_client=0.8.12 bb8=0.9.1"
  echo "stock_bin=sha256:$(sha256sum "$HERE/target/release/sky-lifecycle" | cut -d' ' -f1)"
  echo "fixed_bin=sha256:$(sha256sum "$HERE/fix/p4e-skytable-fixed/target/release/sky-lifecycle" | cut -d' ' -f1)"
  echo "fixed_skytable_sha=sha256:$(sha256sum "$HERE/fix/skytable-0.8.12/src/io/aio.rs" "$HERE/fix/skytable-0.8.12/src/pool.rs" | sha256sum | cut -d' ' -f1)"
  echo "patch_files=fix/skytable-0.8.12/src/io/aio.rs,fix/skytable-0.8.12/src/pool.rs"
  echo "cert_sha256=$(sha256sum "$CERTDIR/cert.pem" | cut -d' ' -f1)"
  echo "cut_ms=10 reply_delay_ms=50"
  echo "server_password=test-only, 23 chars, not logged"
  echo "proxy=dropwise::net::Proxy (plain TCP forwarder, 50ms reply delay; TLS is end-to-end through it)"
} > "$LOGS/environment.txt"

: > "$LOGS/timings.txt"
STATUS=0
run_one() {
  local bin="$1" label="$2" expect="$3" mode="$4"
  local start; start="$(date +%s%N)"
  set +e
  timeout 60 "$bin" "$mode" "$expect" > "$LOGS/$label-$mode.log" 2>&1
  local rc=$?
  set -e
  local ms=$(( ( $(date +%s%N) - start ) / 1000000 ))
  echo "$label $mode rc=$rc ms=$ms" >> "$LOGS/timings.txt"
  if [ "$label" = fixed ] && [ "$rc" -ne 0 ]; then
    STATUS=1
    echo "FIXED_FAIL $mode rc=$rc" >&2
  fi
  if [ "$label" = original ] && ! grep -q 'outcome=Stale' "$LOGS/$label-$mode.log"; then
    STATUS=1
    echo "CONTROL_MISS $mode" >&2
  fi
}

STOCK="$HERE/target/release/sky-lifecycle"
FIXED="$HERE/fix/p4e-skytable-fixed/target/release/sky-lifecycle"
for mode in direct pool2 tls-direct tls-pool; do
  echo "== $mode stock =="
  run_one "$STOCK" stock original "$mode"
  echo "== $mode fixed =="
  run_one "$FIXED" fixed fixed "$mode"
done

echo "== outcomes =="
for f in "$LOGS"/stock-*.log "$LOGS"/fixed-*.log; do
  [ -e "$f" ] || continue
  echo "  $(basename "$f" .log):"
  sed -n 's/^LIFECYCLE /    /p' "$f"
done
if [ "$STATUS" -ne 0 ]; then
  echo "LIFECYCLE_TEST_FAIL" >&2
else
  echo "LIFECYCLE_TEST_OK"
fi
exit "$STATUS"
