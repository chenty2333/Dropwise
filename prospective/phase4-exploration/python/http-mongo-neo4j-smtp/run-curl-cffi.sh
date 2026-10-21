#!/usr/bin/env bash
# curl-cffi 0.16.3: local asyncio HTTP/1.1 keep-alive server + read-only DelayProxy.
# Reuses the shared 3.13 venv (curl-cffi already installed); installs nothing.
# Four levels x 3 reps; fresh logs/run.XXXXXX, never overwritten.
set -uo pipefail
STATUS=0
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
from datetime import datetime, timezone
from importlib import metadata
from pathlib import Path
import curl_cffi
pkgs = {}
for name in ("curl-cffi", "cffi", "pycparser", "certifi", "typing-extensions"):
    try:
        pkgs[name] = metadata.version(name)
    except metadata.PackageNotFoundError:
        pkgs[name] = "missing"
lines = [
    f"date_utc={datetime.now(timezone.utc).isoformat()}",
    f"python={sys.version.split()[0]} ({sys.executable})",
    f"platform={platform.platform()}",
    "venv=shared 3.13 venv already containing curl-cffi 0.16.3 (no install, no upgrade)",
    f"curl_cffi={curl_cffi.__version__} curl={curl_cffi.__curl_version__}",
    f"packages={json.dumps(pkgs, sort_keys=True)}",
    f"http_server=local asyncio HTTP/1.1 keep-alive process, 127.0.0.1:{sys.argv[2]}",
    "pending=research/python-retro/cases/common.py DelayProxy, delay=0.05 s per forwarded chunk (both directions)",
    "client=AsyncSession(max_clients=1, timeout=2.0, trust_env=False, http_version=CurlHttpVersion.V1_1)",
    "levels=B1,B2,B3,M1; M1 Expect.ATOMIC; pre/post 2 samples per side",
    "op_bound=2.0 s, engine_timeout=15.0 s, reps=3",
    "coverage_limit=libcurl performs the transfer in C inside AsyncCurl callbacks; only Python awaits of the target are enumerable",
]
out = Path(sys.argv[1])
out.write_text("\n".join(lines) + "\n", encoding="utf-8")
print("\n".join(lines))
PYEOF

for level in B1 B2 B3 M1; do
  echo "running $level"
  timeout 300 "$PY" -m cases.curl_cffi_case \
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
