#!/usr/bin/env bash
# Focused follow-up: pooled target cancelled after bb8 validation completed
# (~50 ms) and while the update reply is pending. New logs/run.XXXXXX only.
set -eo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/../runner-common.sh"
new_logdir "$HERE"
CID=""; NAME="dw-r3-skycut70-$$"
cleanup() { if [ -n "$CID" ]; then docker logs "$CID" > "$LOGS/container.log" 2>&1 || true; docker stop "$CID" >/dev/null 2>&1 || true; CID=""; fi; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
CID="$(docker run -d --rm --name "$NAME" -p 127.0.0.1::2003 skytable/skytable:v0.8.4)"
PASS=""
for _ in $(seq 1 60); do PASS="$(docker logs "$CID" 2>&1 | sed -n 's/Generated Password: //p' | head -1)"; [ -n "$PASS" ] && break; sleep 0.5; done
PORT="$(docker port "$CID" 2003/tcp | head -1 | sed 's/.*://')"
export DW_SKY_SERVER="127.0.0.1:$PORT" DW_SKY_USER=root DW_SKY_PASS="$PASS"
for _ in $(seq 1 60); do if "$HERE/target/release/sky-ladder" wait 2>/dev/null; then break; fi; sleep 0.5; done
{
  echo "date=$(date -Is)"; echo "container=$NAME"; echo "image=skytable/skytable:v0.8.4"
  echo "image_id=$(docker image inspect skytable/skytable:v0.8.4 --format '{{.Id}}')"
  echo "port=127.0.0.1:$PORT"; echo "skytable_client=0.8.12 bb8=0.9.1"
  echo "purpose=pooled cut after validation, during update reply; cut_ms=70 reply_delay_ms=50"
} > "$LOGS/environment.txt"
for rep in 6 7 8; do
  RUST_LOG=debug "$HERE/target/release/sky-deep" pooled 70 50 "$rep" > "$LOGS/deep-pooled-cut70-rep$rep.stdout.log" 2> "$LOGS/deep-pooled-cut70-rep$rep.stderr.log"
  echo "rep$rep rc=$?" >> "$LOGS/timings.txt"
done
echo "done logs=$LOGS"
