#!/usr/bin/env bash
# Authoritative run for the ordinary-failure vs one-sided-interruption comparison.
#
#   ./failure_compare/run.sh                 # both drivers, 3 reps
#   REPS=2 DRIVER=psycopg ./failure_compare/run.sh
#
# PostgreSQL: the live server on 127.0.0.1:32870 by default (postgres/dw/dw).
# Every trial creates its own table and drops it; the tested handle goes through
# the read-only retro DelayProxy, the observer is a direct connection.
set -euo pipefail
cd "$(dirname "$0")/.."                       # .../python/pg-mysql
REPO=$(cd ../../../.. && pwd)                 # repository root
export PYTHONPATH=".:$REPO/research/python-retro"

PY=${DW_PYTHON:-/home/ava/dropwise-py-work-r3/venv313/bin/python}
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="failure_compare/logs/run.fc.$STAMP"
mkdir -p "$OUT"

echo "OUT $OUT" | tee "$OUT/run.log"
"$PY" failure_compare/compare.py \
  --driver "${DRIVER:-both}" \
  --port "${DW_PG_PORT:-32870}" \
  --reps "${REPS:-3}" \
  --outdir "$OUT" 2>&1 | tee -a "$OUT/run.log"

echo "DONE logs=$OUT"
