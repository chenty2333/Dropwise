#!/usr/bin/env bash
# Build a disposable copy of async-imap 0.12.0 for the round-2 APPEND tests.
#
# Usage:
#   apply-round2-patch.sh [--no-fix] DEST
#
# --no-fix copies the published source and only injects the round-2 tests, so
# the regression tests can be shown to fail before the fix. Without it, the
# APPEND-continuation patch is applied first.
#
# The source is the cargo-registry copy of async-imap 0.12.0. DEST must not
# exist. No file in the repository dependency graph is modified.
set -eo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROUND2_DIR="$(cd "$HERE/.." && pwd)"
FIX=1
if [ "$1" = "--no-fix" ]; then
  FIX=0
  shift
fi
if [ "$#" -ne 1 ]; then
  echo "usage: $0 [--no-fix] DEST" >&2
  exit 2
fi
DEST="$1"
if [ -e "$DEST" ]; then
  echo "destination already exists: $DEST" >&2
  exit 1
fi

if [ -n "$ASYNC_IMAP_SRC" ]; then
  SRC="$ASYNC_IMAP_SRC"
else
  SRC=""
  for candidate in "$HOME"/.cargo/registry/src/*/async-imap-0.12.0; do
    if [ -f "$candidate/Cargo.toml" ]; then
      SRC="$candidate"
      break
    fi
  done
fi
if [ -z "$SRC" ] || [ ! -f "$SRC/Cargo.toml" ]; then
  echo "async-imap 0.12.0 source not found; set ASYNC_IMAP_SRC" >&2
  exit 1
fi
grep -q '^name = "async-imap"' "$SRC/Cargo.toml"
grep -q '^version = "0.12.0"' "$SRC/Cargo.toml"

mkdir -p "$DEST"
cp -a "$SRC/." "$DEST/"
# A lockfile makes the test build reproducible with --offline in this
# environment. The registry crate has no lockfile.
if [ -f "$ROUND2_DIR/Cargo.lock" ]; then
  cp "$ROUND2_DIR/Cargo.lock" "$DEST/Cargo.lock"
fi

if [ "$FIX" -eq 1 ]; then
  patch -p1 -d "$DEST" < "${APPEND_PATCH:-$HERE/append-continuation.review.patch}"
else
  echo "fix=not-applied (published 0.12.0 APPEND continuation read)"
fi

python3 - "$DEST/src/client.rs" "$HERE/append-continuation-tests.round2.rs" <<'PY'
import sys

client_path, tests_path = sys.argv[1], sys.argv[2]
client = open(client_path).read()
tests = open(tests_path).read().rstrip()
if "mod tests {" not in client:
    raise SystemExit("client.rs: mod tests not found")
stripped = client.rstrip()
if not stripped.endswith("}"):
    raise SystemExit("client.rs: expected final closing brace")
insert_at = stripped.rfind("\n}")
if insert_at < 0:
    raise SystemExit("client.rs: could not find final tests brace")
updated = stripped[:insert_at] + "\n" + tests + stripped[insert_at:] + "\n"
open(client_path, "w").write(updated)
PY

echo "dest=$DEST"
echo "source=$SRC"
echo "fix=$FIX"
