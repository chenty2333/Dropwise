#!/usr/bin/env bash
# Round-2 SMTP experiments: bounded M1 with concrete codes + third send,
# #0@3 wire diagnosis with async-smtp debug logs, and old-vs-new-transport
# recovery contrast. Keeps round-1 run.sh and logs untouched.
set -uo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$DIR"

IMAGE=greenmail/standalone:2.1.14
NAME="dw-p4-smtp-r2-$$"
REPS="${REPS:-1 2 3}"
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
export SCAN_VARIANT="async-smtp-0.10.2-round2"

{
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "docker=$(docker --version)"
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect --format '{{.Id}}' "$IMAGE")"
  echo "image_digests=$(docker image inspect --format '{{json .RepoDigests}}' "$IMAGE")"
  echo "container=$NAME smtp=$DW_SMTP_ADDR http=$DW_HTTP_BASE"
  echo "dropwise_commit=$(git -C "$DIR/../../../.." rev-parse HEAD)"
  echo "round2_send_bound=2s"
  echo "cargo_tree:"
  cargo tree --prefix none 2>/dev/null | grep -E '^(async-smtp|env_logger|dropwise|tokio|ureq|serde_json) ' || true
} > "$LOGS/environment-round2.txt"

echo "building"
if ! cargo build --tests > "$LOGS/round2-build.log" 2>&1; then
  echo "build failed" >&2
  tail -80 "$LOGS/round2-build.log" >&2
  exit 2
fi

run_step() {
  local name=$1
  shift
  echo "running $name"
  timeout 300 "$@" > "$LOGS/round2-$name.out.log" 2> "$LOGS/round2-$name.err.log"
  local status=$?
  echo "$status" > "$LOGS/round2-$name.status"
  local expected=0
  case "$name" in
    level-*|m1-*)
      if [[ "$status" -eq 1 ]] && grep -q '^LADDER .*baseline_errors=0 ' "$LOGS/round2-$name.out.log" && grep -q '^LADDER-VIOLATION ' "$LOGS/round2-$name.out.log"; then
        expected=1
      fi ;;
  esac
  if [[ "$status" -ne "$expected" ]]; then
    FAILED=1
    echo "$name exited unexpectedly: $status" >&2
    tail -5 "$LOGS/round2-$name.err.log" >&2 || true
  fi
  return 0
}

for rep in $REPS; do
  run_step "calibration-rep$rep" cargo test --quiet --test levels -- round2-calibration
done
for rep in $REPS; do
  run_step "m1-rep$rep" cargo test --quiet --test levels -- round2-m1 "$rep"
done

# The debug log is the command/reply trace; it is not parsed as a verdict.
echo "running diagnose"
RUST_LOG=async_smtp=debug timeout 300 cargo test --quiet --test levels -- round2-diagnose \
  > "$LOGS/round2-diagnose.out.log" 2> "$LOGS/round2-diagnose.err.log"
diagnose_status=$?
echo "$diagnose_status" > "$LOGS/round2-diagnose.status"
[[ "$diagnose_status" -eq 0 ]] || FAILED=1

for rep in $REPS; do
  run_step "poison-rep$rep" cargo test --quiet --test levels -- round2-poison "$rep"
done

grep -h -E '^(LADDER|ROUND2-CAL|ROUND2-POISON summary|ROUND2-DIAG realized|PREP-)' "$LOGS"/round2-*.out.log > "$LOGS/round2-SUMMARY.txt" 2>/dev/null || true
echo "results:"
cat "$LOGS/round2-SUMMARY.txt" 2>/dev/null || true
echo "statuses:"
for status in "$LOGS"/round2-*.status; do
  [[ -e "$status" ]] || continue
  printf '%s=%s\n' "$(basename "$status" .status)" "$(cat "$status")"
done
echo "logs in $DIR/logs"

exit "$FAILED"
