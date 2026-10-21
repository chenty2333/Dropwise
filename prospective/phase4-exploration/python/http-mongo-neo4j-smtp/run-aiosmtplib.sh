#!/usr/bin/env bash
# aiosmtplib 5.1.3 + GreenMail 2.1.14 (SMTP 3025, REST 8080), loopback only.
# One long-lived SMTP object; no reconnect between target and survivor.
# Four levels x 3 reps; fresh mailbox per scenario instance. Logs in logs/run.XXXXXX.
set -uo pipefail
STATUS=0
export PYTHONDONTWRITEBYTECODE=1

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$DIR" || exit 2
VENV=${DW_VENV:-$HOME/dropwise-py-work-r3/venv-py313}
PY="$VENV/bin/python"
[[ -x "$PY" ]] || { echo "missing venv python: $PY" >&2; exit 2; }
IMAGE=greenmail/standalone:2.1.14
NAME="dw-r3-py-smtp-$$"
mkdir -p "$DIR/logs" || exit 2
LOGS=$(mktemp -d "$DIR/logs/run.XXXXXX") || exit 2
echo "logs=$LOGS"

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "image $IMAGE missing locally" >&2; exit 2;
}
docker run -d --rm --name "$NAME" \
  -p 127.0.0.1::3025 -p 127.0.0.1::8080 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=smtpuser:pass@example.com' \
  "$IMAGE" >/dev/null || { echo "docker run failed" >&2; exit 2; }

SMTP_PORT=$(docker port "$NAME" 3025 | head -1 | sed 's/.*://')
HTTP_PORT=$(docker port "$NAME" 8080 | head -1 | sed 's/.*://')
if [[ -z "$SMTP_PORT" || -z "$HTTP_PORT" ]]; then
  echo "could not discover published ports" >&2
  docker logs "$NAME" >&2 || true
  exit 2
fi

ready=0
for _ in $(seq 1 80); do
  if curl -sf "http://127.0.0.1:$HTTP_PORT/api/user" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.5
done
if [[ "$ready" -ne 1 ]]; then
  echo "GreenMail REST did not become ready" >&2
  docker logs "$NAME" >&2 || true
  exit 2
fi

{
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "python=$("$PY" -V 2>&1) at $PY"
  "$PY" - <<'PYEOF'
import aiosmtplib, httpx, sys
print(f"aiosmtplib={aiosmtplib.__version__} httpx={httpx.__version__} python={sys.version.split()[0]}")
PYEOF
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect --format '{{.Id}}' "$IMAGE")"
  echo "image_digests=$(docker image inspect --format '{{json .RepoDigests}}' "$IMAGE")"
  echo "container=$NAME smtp=127.0.0.1:$SMTP_PORT rest=http://127.0.0.1:$HTTP_PORT"
  echo "pending=DelayProxy delay=0.05s per forwarded chunk, one per scenario instance"
  echo "op_bound=2.0s engine_timeout=15.0 reps=3; no reconnect between target and survivor"
} >"$LOGS/environment.txt"

for level in B1 B2 B3 M1; do
  echo "running $level"
  timeout 420 "$PY" -m cases.aiosmtplib_case \
    --url "smtp://127.0.0.1:$SMTP_PORT" \
    --http "http://127.0.0.1:$HTTP_PORT" \
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
