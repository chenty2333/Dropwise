#!/usr/bin/env bash
# Run a command against a disposable, loopback-only Skytable 0.8.4 instance.
set -euo pipefail
pass="${SKY_PASS:-online-guards-pass}"
name="og-sky-$$"
cid=$(docker run -d --rm --name "$name" -p 127.0.0.1::2003 \
  --entrypoint skyd skytable/skytable:v0.8.4 \
  --mode dev --auth-root-password "$pass" --endpoint tcp@0.0.0.0:2003)
cleanup() { docker rm -f "$cid" >/dev/null 2>&1 || true; }
trap cleanup EXIT
for _ in $(seq 1 200); do
  docker logs "$cid" 2>&1 | grep -q "listening on" && break
  sleep 0.05
done
port=$(docker port "$cid" 2003/tcp 2>/dev/null | head -1 | sed 's/.*://')
if [ -z "$port" ]; then
  echo "skytable did not start" >&2
  docker logs "$cid" 2>&1 | tail -5 >&2
  exit 1
fi
SKY_PORT="$port" SKY_PASS="$pass" "$@"
