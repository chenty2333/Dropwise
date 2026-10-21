#!/usr/bin/env bash
# Shared lifecycle helpers; callers own their container cleanup and EXIT trap.
# User-supplied paths are parents, never directories to delete or overwrite.
new_workdir() {
  local parent="${1:-${TMPDIR:-/tmp}}"
  mkdir -p -- "$parent" || return
  WORK=$(mktemp -d "$parent/dropwise-run.XXXXXX") || return
}
cleanup_workdir() {
  if [[ -n "${WORK:-}" ]]; then
    python3 - "$WORK" <<'CLEANUP'
import shutil, sys
shutil.rmtree(sys.argv[1])
CLEANUP
    WORK=""
  fi
}
new_logdir() {
  local parent="${LOG_DIR:-$1/logs}"
  mkdir -p -- "$parent" || return
  LOGS=$(mktemp -d "$parent/run.XXXXXX") || return
  echo "logs=$LOGS"
}
