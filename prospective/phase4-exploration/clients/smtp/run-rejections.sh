#!/usr/bin/env bash
# Self-contained scripted peer: no Docker, server account or external network.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
new_workdir "${ROUND2_WORK:-}"
trap cleanup_workdir EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cargo test --locked --manifest-path "$HERE/Cargo.toml" --test rejections
SRC="${ASYNC_SMTP_SRC:-}"
if [[ -z "$SRC" ]]; then
  for candidate in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/async-smtp-0.10.2; do
    [[ -f "$candidate/Cargo.toml" ]] && { SRC="$candidate"; break; }
  done
fi
[[ -f "$SRC/Cargo.toml" ]] || { echo 'async-smtp source missing' >&2; exit 2; }
cp -a "$SRC" "$WORK/async-smtp"
patch -p1 -d "$WORK/async-smtp" < "$HERE/patches/async-smtp-poison.diff"
cp "$HERE/Cargo.lock" "$WORK/Cargo.lock"
python3 - "$HERE" "$ROOT" "$WORK" <<'MANIFEST'
from pathlib import Path
import sys
here, root, work = map(Path, sys.argv[1:])
s = (here / 'Cargo.toml').read_text().split('[[test]]')[0]
s = s.replace('path = "../../../.."', f'path = "{root}"')
s += f'\n[[test]]\nname = "rejections"\npath = "{here}/tests/rejections.rs"\n'
s += '\n[patch.crates-io]\nasync-smtp = { path = "async-smtp" }\n'
(work / 'Cargo.toml').write_text(s)
MANIFEST
SMTP_POISON_PATCH=1 CARGO_TARGET_DIR="$HERE/target/rejections-fixed" cargo test --offline --manifest-path "$WORK/Cargo.toml" --test rejections
