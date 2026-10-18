#!/usr/bin/env bash
# Rebuild the four calibration trees and run the scanner on them.
#
#   bash prospective/candidate-scan/scanner/calibrate.sh
#
# Unpatched: lettre 0.11.23, bb8 0.9.1, sqlx-mysql 0.9.0, bb8-memcached 0.7.0.
# Patched:   the repository's proposed diffs (lettre PR #1171 equivalent,
#            sqlx-mysql begin diff, bb8 PR #325 equivalent).
# Sources are fetched into ~/dropwise-scan-src/ and never built.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
ROOT="${DSCAN_ROOT:-$HOME/dropwise-scan-src}"
CAL="$ROOT/calibration"

python3 -I "$HERE/collect.py" --root "$ROOT" --no-universe --only \
  bb8,lettre,sqlx-mysql,bb8-memcached >/dev/null

rm -rf "$CAL"
mkdir -p "$CAL"
for n in lettre-0.11.23 bb8-0.9.1 sqlx-mysql-0.9.0 bb8-memcached-0.7.0; do
  cp -a "$ROOT/src/$n" "$CAL/$n"
done
cp -a "$CAL/lettre-0.11.23" "$CAL/lettre-0.11.23+fixed"
cp -a "$CAL/bb8-0.9.1" "$CAL/bb8-0.9.1+fixed"
cp -a "$CAL/sqlx-mysql-0.9.0" "$CAL/sqlx-mysql-0.9.0+fixed"

(cd "$CAL/lettre-0.11.23+fixed" && patch -p1 -s < "$REPO/prospective/scan-clients/patches/lettre-send-in-progress.diff")
(cd "$CAL/sqlx-mysql-0.9.0+fixed" && patch -p1 -s < "$REPO/prospective/scan-clients/patches/sqlx-mysql-begin.diff")
(cd "$CAL/bb8-0.9.1+fixed" && patch -p1 -s < "$REPO/prospective/t2-bb8/patches/bb8-invalid-until-validated.diff")

python3 -I "$HERE/scan.py" --root "$CAL" --out "$HERE/../calibration-hits.csv" --quiet
echo "wrote $HERE/../calibration-hits.csv"
