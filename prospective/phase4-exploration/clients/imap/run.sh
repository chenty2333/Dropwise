#!/usr/bin/env bash
# Phase 4 exploration: async-imap 0.12.0 direct Session, cancelled SELECT INBOX.
# Starts one disposable loopback-only GreenMail container, runs the preflight,
# the public-API triage probe, and the B1/B2/B3/M1 ladder three times.
# Does not commit, publish or touch anything outside this client directory.
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"
source "$DIR/../runner-common.sh"
new_logdir "$DIR" || exit 2

IMAGE="greenmail/standalone:2.1.14"
NAME="dw-p4-imap-$$"
CID=""
SMTP=""; IMAP=""; REST=""

cleanup() {
  if [ -n "$CID" ]; then
    docker logs "$CID" > "$LOGS/greenmail.log" 2>&1 || true
    docker stop "$CID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== build =="
if ! cargo build --release --locked 2>&1 | tee "$LOGS/build.log"; then
  echo "BUILD FAILED" >&2
  exit 1
fi

if docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
  echo "container name $NAME already exists" >&2
  exit 1
fi

echo "== start $IMAGE as $NAME =="
CID=$(docker run -d --rm --name "$NAME" \
  -p 127.0.0.1::3025 -p 127.0.0.1::3143 -p 127.0.0.1::8080 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=imapuser:pass@example.com' \
  "$IMAGE")
if [ -z "$CID" ]; then
  echo "docker run failed" >&2
  exit 1
fi

SMTP=$(docker port "$CID" 3025/tcp | head -1 | sed 's/.*://')
IMAP=$(docker port "$CID" 3143/tcp | head -1 | sed 's/.*://')
REST=$(docker port "$CID" 8080/tcp | head -1 | sed 's/.*://')
if [ -z "$SMTP" ] || [ -z "$IMAP" ] || [ -z "$REST" ]; then
  echo "could not read all container ports" >&2
  exit 1
fi
export P4_SMTP_PORT="$SMTP" P4_IMAP_PORT="$IMAP" P4_REST_PORT="$REST"

{
  echo "container_name=$NAME"
  echo "container_id=$CID"
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}')"
  echo "repo_digests=$(docker image inspect "$IMAGE" --format '{{json .RepoDigests}}')"
  echo "smtp=127.0.0.1:$SMTP"
  echo "imap=127.0.0.1:$IMAP"
  echo "rest=127.0.0.1:$REST"
} > "$LOGS/container.txt"
cat "$LOGS/container.txt"

echo "== wait for GreenMail REST =="
ready=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:$REST/api/user" >/dev/null 2>&1; then ready=1; break; fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  echo "GreenMail REST did not become ready" >&2
  docker logs "$CID" 2>&1 | tail -50 >&2 || true
  exit 1
fi
curl -sf "http://127.0.0.1:$REST/api/user" > "$LOGS/greenmail-users.json" 2>&1 || true
docker logs "$CID" > "$LOGS/greenmail-startup.log" 2>&1 || true

IMAGE_ID=$(docker image inspect "$IMAGE" --format '{{.Id}}')
REPO_DIGESTS=$(docker image inspect "$IMAGE" --format '{{json .RepoDigests}}')
GM_VERSION=$(grep -m1 'Starting GreenMail standalone' "$LOGS/greenmail-startup.log" || echo 'GreenMail version not found in startup log')
LOCK_VERSIONS=$(python3 - <<'PY'
import re
s=open('Cargo.lock').read()
for name in ['async-imap','imap-proto','tokio','futures-util','serde_json','dropwise']:
    m=re.search(r'\[\[package\]\]\nname = "'+re.escape(name)+r'"\nversion = "([^"]+)"', s)
    print(f'{name}: {m.group(1) if m else "not found"}')
PY
)
{
  echo "generated_at=$(date -Is)"
  echo "command=bash prospective/phase4-exploration/clients/imap/run.sh"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "docker=$(docker --version)"
  echo "uname=$(uname -srm)"
  echo "crate=async-imap 0.12.0 (default-features=false, features=[runtime-tokio])"
  echo "$LOCK_VERSIONS"
  echo "dropwise_source=path ../../../.. (workspace package dropwise 0.1.0)"
  echo "server_image=$IMAGE"
  echo "server_image_id=$IMAGE_ID"
  echo "server_repo_digests=$REPO_DIGESTS"
  echo "$GM_VERSION"
  echo "greenmail_test_setup=-Dgreenmail.setup.test.all -Dgreenmail.auth.disabled -Dgreenmail.users=imapuser:pass@example.com"
  echo "observer=GET http://127.0.0.1:<rest>/api/user/imapuser@example.com/messages (direct, no proxy)"
  echo "pending=dropwise::net::Proxy reply_delay=50ms, Flavor::MultiThread{workers:1}"
  echo "config=races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0"
  echo "repetitions=3 (ladder 1,2,3 separate processes; one container for the runner)"
} > "$LOGS/environment.txt"
cat "$LOGS/environment.txt"

run_phase() {
  local name="$1"
  shift
  echo "== $name =="
  local start end rc
  start=$(date +%s)
  "$@" > "$LOGS/$name.stdout.log" 2> "$LOGS/$name.stderr.log"
  rc=$?
  end=$(date +%s)
  echo "$name rc=$rc seconds=$((end-start))" | tee -a "$LOGS/timings.txt"
  cat "$LOGS/$name.stdout.log"
  if [ -s "$LOGS/$name.stderr.log" ]; then
    echo "--- $name stderr ---"
    cat "$LOGS/$name.stderr.log"
  fi
  return "$rc"
}

BIN="./target/release/imap-explore"

run_phase "preflight" "$BIN" preflight || { echo "PREFLIGHT FAILED" >&2; exit 1; }
run_phase "probe" "$BIN" probe || { echo "PROBE FAILED" >&2; exit 1; }

for rep in 1 2 3; do
  run_phase "round-$rep" "$BIN" ladder "$rep" || { echo "LADDER REPETITION $rep FAILED" >&2; exit 1; }
done

grep -h '^LADDER' "$LOGS"/round-*.stdout.log > "$LOGS/ladder-summary.txt" || true
for f in "$LOGS"/round-*.stdout.log; do
  rep=$(basename "$f" .stdout.log)
  sed -n "s/^LADDER /LADDER $rep /p" "$f"
done > "$LOGS/ladder-summary-by-rep.txt" || true
echo "== ladder summary =="
cat "$LOGS/ladder-summary.txt"

echo "== done; container will be stopped by trap =="
