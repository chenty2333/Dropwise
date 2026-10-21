#!/usr/bin/env bash
# Round-2 dependency-copy checks. Creates two disposable copies of
# async-imap 0.12.0 under /tmp: published APPEND code + tests, and patched
# APPEND code + tests. The published copy must fail exactly the stale-response
# tests; the patched copy must pass the whole suite. Also runs fmt/clippy.
set -eo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/../../runner-common.sh"
new_logdir "$HERE/.."
new_workdir "${ROUND2_TEST_WORK:-}"
trap cleanup_workdir EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
NOFIX="$WORK/async-imap-0.12.0-published"
FIX="$WORK/async-imap-0.12.0-round2-patched"
STATUS=0

echo "== prepare copies =="
"$HERE/apply-round2-patch.sh" --no-fix "$NOFIX" > "$LOGS/round2-patch-apply-nofix.log" 2>&1
"$HERE/apply-round2-patch.sh" "$FIX" > "$LOGS/round2-patch-apply-fix.log" 2>&1
cat "$LOGS/round2-patch-apply-nofix.log"
cat "$LOGS/round2-patch-apply-fix.log"

echo "== published code: upstream tests with round2_ skipped =="
set +e
(
  cd "$NOFIX"
  cargo test --offline --no-default-features --features runtime-tokio --lib -- --skip round2_
) > "$LOGS/round2-patch-tests-upstream-unpatched.log" 2>&1
UP_RC=$?
set -e
tail -3 "$LOGS/round2-patch-tests-upstream-unpatched.log"
if [ "$UP_RC" -ne 0 ] || ! grep -q '73 passed; 0 failed' "$LOGS/round2-patch-tests-upstream-unpatched.log"; then
  echo "FAIL: published upstream tests did not pass 73/73" >&2
  STATUS=1
fi

echo "== published code: round2 regression tests (expected failures) =="
set +e
(
  cd "$NOFIX"
  cargo test --offline --no-default-features --features runtime-tokio --lib round2_
) > "$LOGS/round2-patch-tests-unpatched.log" 2>&1
NOFIX_RC=$?
set -e
tail -12 "$LOGS/round2-patch-tests-unpatched.log"
if ! grep -q '11 passed; 4 failed' "$LOGS/round2-patch-tests-unpatched.log"; then
  echo "FAIL: expected 11 passed / 4 failed on published code" >&2
  STATUS=1
fi
if [ "$NOFIX_RC" -eq 0 ]; then
  echo "FAIL: published-code regression tests unexpectedly passed" >&2
  STATUS=1
fi

echo "== patched code: full suite =="
set +e
(
  cd "$FIX"
  cargo test --offline --no-default-features --features runtime-tokio --lib
) > "$LOGS/round2-patch-tests-patched.log" 2>&1
FIX_RC=$?
set -e
tail -6 "$LOGS/round2-patch-tests-patched.log"
if [ "$FIX_RC" -ne 0 ] || ! grep -q '88 passed; 0 failed' "$LOGS/round2-patch-tests-patched.log"; then
  echo "FAIL: patched suite did not pass 88/88" >&2
  STATUS=1
fi

echo "== fmt =="
set +e
(
  cd "$FIX"
  cargo fmt --all -- --check
) > "$LOGS/round2-patch-fmt-patched.log" 2>&1
FMT_RC=$?
(
  cd "$NOFIX"
  cargo fmt --all -- --check
) > "$LOGS/round2-patch-fmt-unpatched.log" 2>&1
FMT_U_RC=$?
set -e
echo "patched_fmt_rc=$FMT_RC unpatched_fmt_rc=$FMT_U_RC"
cat "$LOGS/round2-patch-fmt-patched.log"
if [ "$FMT_RC" -ne 0 ] || [ "$FMT_U_RC" -ne 0 ]; then
  echo "FAIL: cargo fmt --check failed" >&2
  STATUS=1
fi

echo "== clippy =="
set +e
(
  cd "$FIX"
  cargo clippy --offline --no-default-features --features runtime-tokio --all-targets
) > "$LOGS/round2-patch-clippy-patched.log" 2>&1
CLIP_RC=$?
(
  cd "$NOFIX"
  cargo clippy --offline --no-default-features --features runtime-tokio --all-targets
) > "$LOGS/round2-patch-clippy-unpatched.log" 2>&1
CLIP_U_RC=$?
set -e
grep '^warning:' "$LOGS/round2-patch-clippy-patched.log" | sort -u > "$WORK/warnings-patched.txt" || true
grep '^warning:' "$LOGS/round2-patch-clippy-unpatched.log" | sort -u > "$WORK/warnings-unpatched.txt" || true
echo "patched_clippy_rc=$CLIP_RC unpatched_clippy_rc=$CLIP_U_RC"
echo "patched_warning_lines=$(wc -l < "$WORK/warnings-patched.txt") unpatched_warning_lines=$(wc -l < "$WORK/warnings-unpatched.txt")"
if ! diff -u "$WORK/warnings-unpatched.txt" "$WORK/warnings-patched.txt" > "$LOGS/round2-patch-clippy-warning-diff.log"; then
  echo "NOTE: clippy warning sets differ (see round2-patch-clippy-warning-diff.log)" >&2
  cat "$LOGS/round2-patch-clippy-warning-diff.log"
  STATUS=1
fi
if [ "$CLIP_RC" -ne 0 ] || [ "$CLIP_U_RC" -ne 0 ]; then
  echo "FAIL: clippy returned nonzero" >&2
  STATUS=1
fi

{
  echo "# round-2 patch tests"
  echo "date=$(date -Is)"
  echo "work=$WORK"
  echo "published_upstream_rc=$UP_RC"
  echo "published_round2_rc=$NOFIX_RC (expected nonzero)"
  echo "patched_full_rc=$FIX_RC"
  echo "fmt_patched_rc=$FMT_RC"
  echo "fmt_unpatched_rc=$FMT_U_RC"
  echo "clippy_patched_rc=$CLIP_RC"
  echo "clippy_unpatched_rc=$CLIP_U_RC"
  echo "status=$STATUS"
} > "$LOGS/round2-patch-tests-summary.txt"
cat "$LOGS/round2-patch-tests-summary.txt"
exit "$STATUS"
