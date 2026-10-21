#!/usr/bin/env bash
# Round-2 IMAP runner. Uses two disposable GreenMail containers (one per
# dependency variant), loopback-only random ports, and writes only round2-*
# files under clients/imap/logs/. Round-1 files are not touched.
set -eo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"
ROUND2="$HERE/round2"
PATCHED="$HERE/patched"
new_workdir "${ROUND2_WORK:-}"
FAILED=0
CID=""

cleanup() {
  if [ -n "$CID" ]; then
    docker stop "$CID" >/dev/null 2>&1 || true
    CID=""
  fi
  cleanup_workdir
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

{
  echo "# generated $(date -Is)"
  echo "# cwd $ROOT"
  echo "cargo build --release --offline   # round2/, published async-imap 0.12.0"
  echo "patched/apply-round2-patch.sh $WORK/async-imap-patched"
  echo "CARGO_TARGET_DIR=$WORK/target cargo build --release --offline   # copied round2 crate with [patch.crates-io]"
  echo "docker run -d --rm --name dw-p4-imap-r2-<variant>-$$ -p 127.0.0.1::3025 -p 127.0.0.1::3143 -p 127.0.0.1::8080 -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=imapuser:pass@example.com' greenmail/standalone:2.1.14"
} > "$LOGS/round2-commands.txt"
: > "$LOGS/round2-timings.txt"

echo "== build unpatched =="
(
  cd "$ROUND2"
  cargo build --release --offline
) > "$LOGS/round2-build-unpatched.log" 2>&1
BIN_U="$ROUND2/target/release/imap-round2"

echo "== build patched dependency and binary =="
"$PATCHED/apply-round2-patch.sh" "$WORK/async-imap-patched" > "$LOGS/round2-apply-patch.log" 2>&1
mkdir -p "$WORK/round2-patched"
cp -a "$ROUND2/Cargo.toml" "$ROUND2/Cargo.lock" "$ROUND2/src" "$WORK/round2-patched/"
sed -i "s|path = \"../../../../..\"|path = \"$ROOT\"|" "$WORK/round2-patched/Cargo.toml"
cat >> "$WORK/round2-patched/Cargo.toml" <<'EOF'

[patch.crates-io]
async-imap = { path = "../async-imap-patched" }
EOF
sed -i "s|../../../../../support/ladder.rs|$ROOT/prospective/support/ladder.rs|" "$WORK/round2-patched/src/main.rs"
(
  cd "$WORK/round2-patched"
  CARGO_TARGET_DIR="$WORK/target" cargo build --release --offline
  CARGO_TARGET_DIR="$WORK/target" cargo tree --offline -p p4-imap-round2
) > "$LOGS/round2-build-patched.log" 2>&1
BIN_P="$WORK/target/release/imap-round2"

start_greenmail() {
  local suffix="$1"
  local name="dw-p4-imap-r2-$suffix-$$"
  CID="$(docker run -d --rm --name "$name" \
    -p 127.0.0.1::3025 -p 127.0.0.1::3143 -p 127.0.0.1::8080 \
    -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=imapuser:pass@example.com' \
    greenmail/standalone:2.1.14)"
  echo "$CID" > "$WORK/cid-$suffix"
  P4_SMTP_PORT="$(docker port "$CID" 3025/tcp | head -1 | sed 's/.*://')"
  P4_IMAP_PORT="$(docker port "$CID" 3143/tcp | head -1 | sed 's/.*://')"
  P4_REST_PORT="$(docker port "$CID" 8080/tcp | head -1 | sed 's/.*://')"
  export P4_SMTP_PORT P4_IMAP_PORT P4_REST_PORT
  local ready=0
  local i
  for i in $(seq 1 60); do
    if curl -sf "http://127.0.0.1:$P4_REST_PORT/api/user" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  if [ "$ready" -ne 1 ]; then
    echo "GreenMail did not become ready" >&2
    exit 1
  fi
  {
    echo "variant=$suffix"
    echo "container_name=$name"
    echo "container_id=$CID"
    echo "image=greenmail/standalone:2.1.14"
    echo "smtp=127.0.0.1:$P4_SMTP_PORT"
    echo "imap=127.0.0.1:$P4_IMAP_PORT"
    echo "rest=127.0.0.1:$P4_REST_PORT"
  } > "$LOGS/round2-container-$suffix.txt"
  echo "variant $suffix container $CID ports smtp=$P4_SMTP_PORT imap=$P4_IMAP_PORT rest=$P4_REST_PORT"
}

stop_greenmail() {
  local suffix="$1"
  docker logs "$CID" > "$LOGS/round2-greenmail-$suffix.log" 2>&1 || true
  docker stop "$CID" >/dev/null 2>&1 || true
  CID=""
}

run_phase() {
  local name="$1"
  shift
  local start
  start="$(date +%s)"
  set +e
  "$@" > "$LOGS/round2-$name.stdout.log" 2> "$LOGS/round2-$name.stderr.log"
  local rc=$?
  set -e
  local secs=$(( $(date +%s) - start ))
  echo "$name rc=$rc seconds=$secs" >> "$LOGS/round2-timings.txt"
  echo "P4_SMTP_PORT=$P4_SMTP_PORT P4_IMAP_PORT=$P4_IMAP_PORT P4_REST_PORT=$P4_REST_PORT $* > logs/round2-$name.stdout.log" >> "$LOGS/round2-commands.txt"
  if [ "$rc" -ne 0 ]; then
    echo "PHASE FAILED: $name rc=$rc" >&2
    FAILED=1
  fi
  return 0
}

run_variant() {
  local suffix="$1"
  local bin="$2"
  echo "=============================="
  echo "== variant $suffix =="
  echo "=============================="
  start_greenmail "$suffix"
  run_phase "$suffix-preflight" "$bin" preflight
  local rep
  for rep in 1 2 3; do
    run_phase "$suffix-chain-$rep" "$bin" chain "$rep"
  done
  for rep in 1 2 3; do
    run_phase "$suffix-chain-reverse-$rep" "$bin" chain-reverse "$rep"
  done
  run_phase "$suffix-patch-behavior" "$bin" patch-behavior
  local before
  before="$(docker logs "$CID" 2>&1 | wc -l)"
  run_phase "$suffix-search-unselected" "$bin" search-unselected
  sleep 1
  docker logs "$CID" 2>&1 | tail -n +$(( before + 1 )) > "$LOGS/round2-$suffix-search-unselected-server.log" || true
  for rep in 1 2 3; do
    run_phase "$suffix-ladder-$rep" "$bin" ladder "$rep"
  done
  stop_greenmail "$suffix"
}

run_variant unpatched "$BIN_U"
run_variant patched "$BIN_P"

{
  echo "# round-2 IMAP environment"
  echo "date=$(date -Is)"
  echo "repo=$ROOT"
  echo "round2_crate=$ROUND2"
  echo "work=$WORK"
  echo "image=greenmail/standalone:2.1.14"
  echo "image_id=$(docker image inspect greenmail/standalone:2.1.14 --format '{{.Id}}')"
  echo "async_imap=0.12.0 (runtime-tokio, default-features off)"
  echo "tokio=1.x MultiThread workers=1"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms, loopback only"
  echo "config=races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0"
  echo "observer=GreenMail REST GET /api/user/imapuser@example.com/messages direct"
  echo "seed=SMTP direct"
  echo "--- unpatched container ---"
  cat "$LOGS/round2-container-unpatched.txt"
  echo "--- patched container ---"
  cat "$LOGS/round2-container-patched.txt"
  echo "--- build identity ---"
  grep -E 'async-imap v0.12.0' "$LOGS/round2-build-patched.log" | head -2 || true
} > "$LOGS/round2-environment.txt"

unset P4_SMTP_PORT P4_IMAP_PORT P4_REST_PORT
echo "done; work=$WORK failed=$FAILED"
exit "$FAILED"
