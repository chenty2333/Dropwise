#!/usr/bin/env bash
# Reproduce the Gel 7.1 connectivity + single-point cancellation probe.
# One disposable container, loopback-only, insecure dev mode; teardown on exit.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="dw-r3-gel-$$"
IMAGE="geldata/gel:7"
RUN="$HERE/logs/run.$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$RUN"
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker run -d --name "$NAME" -e GEL_SERVER_SECURITY=insecure_dev_mode \
  -p 127.0.0.1::5656 "$IMAGE" >/dev/null
PORT=""
for _ in $(seq 1 120); do
  PORT="$(docker port "$NAME" 5656/tcp 2>/dev/null | awk -F: '{print $NF}')"
  if [[ -n "$PORT" ]] && docker exec "$NAME" gel query --host 127.0.0.1 --port 5656 \
       --user admin --tls-security insecure 'select 1' >/dev/null 2>&1; then break; fi
  sleep 0.5
done
[[ -n "$PORT" ]] || { echo "Gel did not become ready" >&2; exit 1; }
{
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "image=$IMAGE"
  echo "image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}')"
  echo "image_digest=$(docker image inspect "$IMAGE" --format '{{index .RepoDigests 0}}')"
  echo "server=$(docker logs "$NAME" 2>&1 | grep -o 'starting Gel server [^ ]*' | head -1)"
  echo "container=$NAME"
  echo "port=$PORT"
  echo "security=insecure_dev_mode (Trust auth, self-signed TLS, tls_security=Insecure)"
  echo "max_concurrency=1"
  echo "rustc=$(rustc --version)"
} > "$RUN/environment.txt"
( cd "$HERE" && DW_GEL_PORT="$PORT" CARGO_BUILD_JOBS=6 timeout 120 \
    cargo test --offline --test gel_probe -- --nocapture --test-threads=1 ) \
  > "$RUN/probe.log" 2>&1 || { cat "$RUN/probe.log"; exit 1; }
tail -6 "$RUN/probe.log"
echo "logs=$RUN"
