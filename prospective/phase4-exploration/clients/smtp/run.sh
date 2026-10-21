#!/usr/bin/env bash
# Phase 4 exploration: async-smtp 0.10.2 direct handle against disposable
# GreenMail 2.1.14. Starts one loopback-only container, runs the pre-ladder
# evidence and the B1/B2/B3/M1 ladder, and removes the container on exit.
#
# All paths are relative to this file. Nothing outside this directory is
# modified. The container is disposable; --rm plus the trap avoids leftovers.
set -uo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$DIR"

IMAGE=greenmail/standalone:2.1.14
NAME="dw-p4-smtp-$$"
LEVELS="${LEVELS:-B1 B2 B3 M1}"
REPS="${REPS:-1 2 3}"
RUN_PREP="${RUN_PREP:-1}"
source "$DIR/../runner-common.sh"
new_logdir "$DIR" || exit 2
FAILED=0

cleanup() {
  if [[ -n "${NAME:-}" ]]; then
    docker rm -f "$NAME" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "image $IMAGE is not present locally" >&2
  exit 2
fi

echo "starting $NAME from $IMAGE"
docker run -d --rm --name "$NAME" \
  -p 127.0.0.1::3025 -p 127.0.0.1::8080 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=smtpuser:pass@example.com,dwother:pass@example.com' \
  "$IMAGE" >/dev/null || { echo "docker run failed" >&2; exit 2; }

SMTP_PORT=$(docker port "$NAME" 3025 | head -1 | sed 's/.*://')
HTTP_PORT=$(docker port "$NAME" 8080 | head -1 | sed 's/.*://')
if [[ -z "$SMTP_PORT" || -z "$HTTP_PORT" ]]; then
  echo "could not discover published ports" >&2
  exit 2
fi

ready=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:$HTTP_PORT/api/user" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.5
done
if [[ "$ready" -ne 1 ]]; then
  echo "GreenMail REST API did not become ready" >&2
  docker logs "$NAME" >&2 || true
  exit 2
fi
for _ in $(seq 1 20); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$SMTP_PORT") 2>/dev/null; then
    exec 3>&- 3<&- || true
    break
  fi
  sleep 0.25
done

export DW_SMTP_ADDR="127.0.0.1:$SMTP_PORT"
export DW_HTTP_BASE="http://127.0.0.1:$HTTP_PORT"
export SCAN_VARIANT="async-smtp-0.10.2"

{
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "docker=$(docker --version)"
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect --format '{{.Id}}' "$IMAGE")"
  echo "image_digests=$(docker image inspect --format '{{json .RepoDigests}}' "$IMAGE")"
  echo "greenmail_version=v2.1.14 (from image tag; container log follows)"
  docker logs "$NAME" 2>&1 | grep -m1 'Starting GreenMail standalone' || true
  echo "container=$NAME smtp=$DW_SMTP_ADDR http=$DW_HTTP_BASE"
  echo "dropwise_commit=$(git -C "$DIR/../../../.." rev-parse HEAD)"
  echo "dropwise_path=../../../.. features=net"
  echo "cargo_tree:"
  cargo tree --prefix none 2>/dev/null | grep -E '^(async-smtp|dropwise|tokio|ureq|serde_json) ' || true
} > "$LOGS/environment.txt"

echo "building"
if ! cargo build --tests > "$LOGS/build.log" 2>&1; then
  echo "build failed" >&2
  tail -80 "$LOGS/build.log" >&2
  exit 2
fi

run_step() {
  local name=$1
  shift
  echo "running $name"
  timeout 300 "$@" > "$LOGS/$name.out.log" 2> "$LOGS/$name.err.log"
  local status=$?
  echo "$status" > "$LOGS/$name.status"
  local expected=0
  case "$name" in
    level-*|m1-*)
      if [[ "$status" -eq 1 ]] && grep -q '^LADDER .*baseline_errors=0 ' "$LOGS/$name.out.log" && grep -q '^LADDER-VIOLATION ' "$LOGS/$name.out.log"; then
        expected=1
      fi ;;
  esac
  if [[ "$status" -ne "$expected" ]]; then
    FAILED=1
    echo "$name exited unexpectedly: $status" >&2
    tail -5 "$LOGS/$name.err.log" >&2 || true
  fi
  return 0
}

if [[ "$RUN_PREP" -eq 1 ]]; then
  run_step prep-nocancel cargo test --quiet --test levels -- nocancel
  run_step prep-reuse cargo test --quiet --test levels -- reuse
  run_step prep-calibration cargo test --quiet --test levels -- calibration
fi

for level in $LEVELS; do
  for rep in $REPS; do
    run_step "level-${level}-rep${rep}" cargo test --quiet --test levels -- level "$level" "$rep"
  done
done

grep -h '^LADDER' "$LOGS"/*.out.log > "$LOGS/LADDER.txt" 2>/dev/null || true
echo "results:"
cat "$LOGS/LADDER.txt" 2>/dev/null || true
echo "statuses:"
for status in "$LOGS"/*.status; do
  [[ -e "$status" ]] || continue
  printf '%s=%s\n' "$(basename "$status" .status)" "$(cat "$status")"
done
echo "logs in $DIR/logs"

# The trap removes the disposable container.

exit "$FAILED"
