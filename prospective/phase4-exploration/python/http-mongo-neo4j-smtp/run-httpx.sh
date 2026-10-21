#!/usr/bin/env bash
# httpx 0.28.1: local asyncio HTTP/1.1 keep-alive server + read-only DelayProxy.
# Four levels x 3 reps. Logs go to a fresh logs/run.XXXXXX (never overwritten).
set -uo pipefail
export PYTHONDONTWRITEBYTECODE=1

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$DIR" || exit 2
VENV=${DW_VENV:-$HOME/dropwise-py-work-r3/venv-py313}
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

"$PY" - "$LOGS/environment.txt" "$PORT" <<'PYEOF'
import json, platform, sys
from pathlib import Path
import httpx, httpcore
out = Path(sys.argv[1])
lines = [
    f"date_utc={__import__('datetime').datetime.now(__import__('datetime').timezone.utc).isoformat()}",
    f"python={sys.version.split()[0]} ({sys.executable})",
    f"platform={platform.platform()}",
    f"httpx={httpx.__version__}",
    f"httpcore={httpcore.__version__}",
    f"http_server=local asyncio HTTP/1.1 keep-alive process, 127.0.0.1:{sys.argv[2]}",
    "pending=research/python-retro/cases/common.py DelayProxy, delay=0.05 s per forwarded chunk (both directions)",
    "client_limits=Limits(max_connections=1, max_keepalive_connections=1), http2=False",
    "op_bound=2.0 s, engine_timeout=15.0 s, reps=3",
]
out.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))
PYEOF

for level in B1 B2 B3 M1; do
  echo "running $level"
  timeout 300 "$PY" -m cases.httpx_case \
    --url "http://127.0.0.1:$PORT" \
    --levels "$level" --reps 3 --timeout 15 \
    >"$LOGS/level-$level.log" 2>"$LOGS/level-$level.err"
  echo "$?" >"$LOGS/level-$level.status"
done

echo "=== summary ==="
grep -h '^SUMMARY' "$LOGS"/level-*.log || true
echo "logs=$LOGS"
