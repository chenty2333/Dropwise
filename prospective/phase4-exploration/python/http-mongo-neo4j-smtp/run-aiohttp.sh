#!/usr/bin/env bash
# aiohttp 3.14.5: local asyncio HTTP/1.1 keep-alive server + read-only DelayProxy.
# Dedicated 3.13 venv (~/dropwise-py-work-r3/venv-aiohttp313) so the shared
# venv used by the httpx/curl-cffi runs is not modified.
# Four levels x 3 reps; fresh logs/run.XXXXXX, never overwritten.
set -uo pipefail
STATUS=0
export PYTHONDONTWRITEBYTECODE=1

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$DIR" || exit 2
VENV=${DW_AIOHTTP_VENV:-$HOME/dropwise-py-work-r3/venv-aiohttp313}
PY="$VENV/bin/python"
[[ -x "$PY" ]] || { echo "missing venv python: $PY" >&2; exit 2; }

mkdir -p "$DIR/logs" || exit 2
LOGS=$(mktemp -d "$DIR/logs/run.XXXXXX") || exit 2
echo "logs=$LOGS"

PORTFILE="$LOGS/http-port"
"$PY" "$DIR/http_server.py" --port-file "$PORTFILE" \
  --log-file "$LOGS/http-server.jsonl" \
  >"$LOGS/http-server.out" 2>"$LOGS/http-server.err" &
SRV=$!
cleanup() {
  kill "$SRV" 2>/dev/null || true
  wait "$SRV" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for _ in $(seq 1 100); do
  [[ -s "$PORTFILE" ]] && break
  sleep 0.1
done
if [[ ! -s "$PORTFILE" ]]; then
  echo "HTTP server did not publish a port" >&2
  exit 2
fi
PORT=$(cat "$PORTFILE")

"$PY" - "$LOGS/environment.txt" "$PORT" "$PY" <<'PYEOF'
import json, platform, sys
from datetime import datetime, timezone
from importlib import metadata
from pathlib import Path
import aiohttp
pkgs = {}
for name in ("aiohttp", "aiohappyeyeballs", "aiosignal", "attrs", "frozenlist",
             "idna", "multidict", "propcache", "yarl"):
    try:
        pkgs[name] = metadata.version(name)
    except metadata.PackageNotFoundError:
        pkgs[name] = "missing"
lines = [
    f"date_utc={datetime.now(timezone.utc).isoformat()}",
    f"python={sys.version.split()[0]} ({sys.executable})",
    f"platform={platform.platform()}",
    "venv=fresh 3.13 venv for this target (the shared venv-py313 was not modified)",
    f"aiohttp={aiohttp.__version__} packages={json.dumps(pkgs, sort_keys=True)}",
    f"http_server=local asyncio HTTP/1.1 keep-alive process, 127.0.0.1:{sys.argv[2]}",
    "pending=research/python-retro/cases/common.py DelayProxy, delay=0.05 s per forwarded chunk (both directions)",
    "client=ClientSession(TCPConnector(limit=1, limit_per_host=1, force_close=False, enable_cleanup_closed=False)), auto_decompress=True, trust_env=False",
    "levels=B1,B2,B3,M1; M1 Expect.ATOMIC; pre/post 2 samples per side",
    "op_bound=2.0 s, engine_timeout=15.0 s, reps=3",
]
out = Path(sys.argv[1])
out.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))
PYEOF

for level in B1 B2 B3 M1; do
  echo "running $level"
  timeout 300 "$PY" -m cases.aiohttp_case \
    --url "http://127.0.0.1:$PORT" \
    --levels "$level" --reps 3 --timeout 15 \
    >"$LOGS/level-$level.log" 2>"$LOGS/level-$level.err"
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "$rc" >"$LOGS/level-$level.status"
done

echo "=== summary ==="
grep -h '^SUMMARY' "$LOGS"/level-*.log || true
echo "logs=$LOGS"

exit "$STATUS"
