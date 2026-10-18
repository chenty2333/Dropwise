#!/usr/bin/env bash
# One-click candidate scan: discover reverse dependencies, download .crate
# sources outside the repository, then run the static scanner.
#
#   bash prospective/candidate-scan/scanner/run.sh
#   bash prospective/candidate-scan/scanner/run.sh --limit 150
#
# Nothing downloaded is built or executed.  Sources live in
# ~/dropwise-scan-src/ (override with DSCAN_ROOT).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${DSCAN_ROOT:-$HOME/dropwise-scan-src}"

python3 -I "$HERE/collect.py" --root "$ROOT" "$@"
python3 -I "$HERE/scan.py" --root "$ROOT/src" --out "$HERE/../hits.csv"
echo "wrote $HERE/../hits.csv"
