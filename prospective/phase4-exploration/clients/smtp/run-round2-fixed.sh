#!/usr/bin/env bash
# Round-2 fixed-dependency contrast: apply patches/async-smtp-poison.diff to a
# disposable copy of async-smtp 0.10.2, then run the normal-send calibration and
# the poison/reconnect scenario. The repository copy of async-smtp is untouched.
set -uo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$DIR"

SRC="${ASYNC_SMTP_SRC:-}"
if [[ -z "$SRC" ]]; then
  for candidate in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/async-smtp-0.10.2; do
    [[ -f "$candidate/Cargo.toml" ]] && { SRC="$candidate"; break; }
  done
fi
IMAGE=greenmail/standalone:2.1.14
NAME="dw-p4-smtp-r2f-$$"
REPS="${REPS:-1 2 3}"
source "$DIR/../runner-common.sh"
new_logdir "$DIR" || exit 2
FAILED=0
new_workdir "${ROUND2_WORK:-}" || exit 2
FIXED="$WORK/async-smtp-fixed"
MANIFEST_DIR="$WORK/manifest"

cleanup() {
  if [[ -n "${NAME:-}" ]]; then
    docker rm -f "$NAME" >/dev/null 2>&1 || true
  fi
  cleanup_workdir
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ ! -d "$SRC" ]]; then
  echo "async-smtp 0.10.2 registry source not found at $SRC" >&2
  exit 2
fi
cp -a "$SRC" "$FIXED"
if ! patch -d "$FIXED" -p1 --forward < "$DIR/patches/async-smtp-poison.diff"; then
  echo "failed to apply $DIR/patches/async-smtp-poison.diff" >&2
  exit 2
fi

mkdir -p "$MANIFEST_DIR"
cp "$DIR/Cargo.lock" "$MANIFEST_DIR/Cargo.lock"
cat > "$MANIFEST_DIR/Cargo.toml" <<EOF
[package]
name = "smtp-p4-exploration"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
dropwise = { path = "$DIR/../../../..", features = ["net"] }
async-smtp = { path = "$FIXED" }
tokio = { version = "1", features = ["rt", "rt-multi-thread", "net", "io-util", "time", "macros", "sync"] }
ureq = "2"
serde_json = "1"

[dev-dependencies]
env_logger = "0.11"

[[test]]
name = "levels"
path = "$DIR/tests/levels.rs"
harness = false
EOF

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "image $IMAGE is not present locally" >&2
  exit 2
fi
docker run -d --rm --name "$NAME" \
  -p 127.0.0.1::3025 -p 127.0.0.1::8080 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=smtpuser:pass@example.com,dwother:pass@example.com' \
  "$IMAGE" >/dev/null || { echo "docker run failed" >&2; exit 2; }
SMTP_PORT=$(docker port "$NAME" 3025 | head -1 | sed 's/.*://')
HTTP_PORT=$(docker port "$NAME" 8080 | head -1 | sed 's/.*://')
ready=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:$HTTP_PORT/api/user" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.5
done
[[ "$ready" -eq 1 && -n "$SMTP_PORT" && -n "$HTTP_PORT" ]] || { echo "GreenMail not ready" >&2; exit 2; }
export DW_SMTP_ADDR="127.0.0.1:$SMTP_PORT"
export DW_HTTP_BASE="http://127.0.0.1:$HTTP_PORT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$DIR/target/fixed}"

{
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "fixed_source=$FIXED"
  echo "patch=$DIR/patches/async-smtp-poison.diff"
  echo "container=$NAME smtp=$DW_SMTP_ADDR http=$DW_HTTP_BASE"
  echo "dropwise_commit=$(git -C "$DIR/../../../.." rev-parse HEAD)"
  echo "cargo_tree:"
  cargo tree --manifest-path "$MANIFEST_DIR/Cargo.toml" --prefix none 2>/dev/null | grep -E '^(async-smtp|dropwise|env_logger) ' || true
} > "$LOGS/environment-round2-fixed.txt"

echo "building fixed dependency"
if ! cargo test --manifest-path "$MANIFEST_DIR/Cargo.toml" --quiet --test levels --no-run > "$LOGS/round2-fixed-build.log" 2>&1; then
  echo "fixed build failed" >&2
  tail -80 "$LOGS/round2-fixed-build.log" >&2
  exit 2
fi

run_step() {
  local name=$1
  shift
  echo "running $name"
  timeout 300 "$@" > "$LOGS/round2-fixed-$name.out.log" 2> "$LOGS/round2-fixed-$name.err.log"
  local status=$?
  echo "$status" > "$LOGS/round2-fixed-$name.status"
  local expected=0
  case "$name" in
    level-*|m1-*)
      if [[ "$status" -eq 1 ]] && grep -q '^LADDER .*baseline_errors=0 ' "$LOGS/round2-fixed-$name.out.log" && grep -q '^LADDER-VIOLATION ' "$LOGS/round2-fixed-$name.out.log"; then
        expected=1
      fi ;;
  esac
  if [[ "$status" -ne "$expected" ]]; then
    FAILED=1
    echo "$name exited unexpectedly: $status" >&2
    tail -8 "$LOGS/round2-fixed-$name.err.log" >&2 || true
  fi
  return 0
}

run_step calibration cargo test --manifest-path "$MANIFEST_DIR/Cargo.toml" --quiet --test levels -- round2-calibration
for rep in $REPS; do
  run_step "poison-rep$rep" cargo test --manifest-path "$MANIFEST_DIR/Cargo.toml" --quiet --test levels -- round2-poison "$rep"
done

grep -h -E '^(ROUND2-CAL|ROUND2-POISON)' "$LOGS"/round2-fixed-*.out.log > "$LOGS/round2-fixed-SUMMARY.txt" 2>/dev/null || true
cat "$LOGS/round2-fixed-SUMMARY.txt" 2>/dev/null || true
for status in "$LOGS"/round2-fixed-*.status; do
  [[ -e "$status" ]] || continue
  printf '%s=%s\n' "$(basename "$status" .status)" "$(cat "$status")"
done

exit "$FAILED"
