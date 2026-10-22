#!/usr/bin/env bash
# Reuse of the existing Rust sqlx 0.9.0 future-drop case (t3-sqlx/tests/rescan.rs).
#
# Nothing is recompiled or edited.  The already-built test binaries are executed
# against the live PostgreSQL server; the release binary is the crates.io 0.9.0
# build, the pr4394 binary is the same crate with the upstream fix for sqlx#4393
# applied (prospective/t3-sqlx/patches/sqlx-pr4394-transaction.diff).  Output is
# stored under failure_compare/logs/futuredrop-reuse.<stamp>/.
#
# The case drops query/transaction futures at Pending boundaries (Dropwise's
# Rust engine), which is the Future-drop counterpart of Python Task.cancel; it
# is *not* a Task.cancel and it sends no protocol CancelRequest.
set -uo pipefail
cd "$(dirname "$0")/.."                       # .../python/pg-mysql
REPO=$(cd ../../../.. && pwd)
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="failure_compare/logs/futuredrop-reuse.$STAMP"
mkdir -p "$OUT"

DB_URL=${DATABASE_URL:-"postgres://postgres:dw@127.0.0.1:32870/dw?sslmode=disable"}
REPS=${REPS:-3}
RELEASE_DIR="$REPO/prospective/t3-sqlx/target/debug/deps"
FIXED_DIR=${SQLX_FIXED_DIR:-"$HOME/dropwise-rescan-work/t3-sqlx-pr4394/target/debug/deps"}

release_bin=$(find "$RELEASE_DIR" -maxdepth 1 -type f -name 'rescan-*' -perm -u+x | head -1)
fixed_bin=$(find "$FIXED_DIR" -maxdepth 1 -type f -name 'rescan-*' -perm -u+x 2>/dev/null | head -1)

redacted_url=$(printf '%s' "$DB_URL" | sed -E 's#(://[^:]+:)[^@]+@#\1***@#')
{
  echo "release_bin=$release_bin"
  echo "fixed_bin=$fixed_bin"
  echo "database_url=$redacted_url"
  echo "reps=$REPS"
  if [ -n "$release_bin" ]; then
    echo "release_list:"
    "$release_bin" --list
    sha256sum "$release_bin"
  fi
  if [ -n "$fixed_bin" ]; then
    echo "fixed_list:"
    "$fixed_bin" --list
    sha256sum "$fixed_bin"
  fi
} > "$OUT/environment.txt" 2>&1

run_one() {
  local bin=$1 variant=$2 rep=$3 rc=0
  DATABASE_URL="$DB_URL" SQLX_VARIANT="$variant" timeout 180 \
    "$bin" --nocapture --test-threads=1 > "$OUT/$variant-$rep.log" 2>&1 || rc=$?
  echo "$variant rep $rep: exit $rc"
}

if [ -z "$release_bin" ]; then
  echo "release binary not found under $RELEASE_DIR" >&2
  exit 1
fi
for rep in $(seq "$REPS"); do
  run_one "$release_bin" release "$rep"
  if [ -n "$fixed_bin" ]; then
    run_one "$fixed_bin" pr4394 "$rep"
  fi
done

grep -H '^RESCAN' "$OUT"/*.log 2>/dev/null | sed 's#.*/##' | sort > "$OUT/summary.txt" || true
cat "$OUT/summary.txt"
if [ -z "$fixed_bin" ]; then
  echo "fixed binary not found under $FIXED_DIR; release-only reuse" | tee -a "$OUT/environment.txt"
fi
echo "DONE logs=$OUT"
