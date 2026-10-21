#!/usr/bin/env bash
# Round-4 sqlx runner. One disposable loopback-only PostgreSQL 18 container on
# an ephemeral published port; new logs/run.XXXXXX only.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
source "$HERE/../runner-common.sh"
cd "$HERE"
new_logdir "$HERE"

NAME="dw-r4-sqlx-$$"
CONTAINER=""
cleanup() {
  if [ -n "$CONTAINER" ]; then
    docker logs "$CONTAINER" > "$LOGS/postgres.log" 2>&1 || true
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    CONTAINER=""
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== build =="
cargo build --release --offline > "$LOGS/build.log" 2>&1 || cargo build --release >> "$LOGS/build.log" 2>&1

echo "== postgres =="
CONTAINER="$(docker run -d --rm --name "$NAME" \
  -p 127.0.0.1::5432 \
  -e POSTGRES_USER=dropwise \
  -e POSTGRES_PASSWORD=dropwise \
  -e POSTGRES_DB=dropwise \
  postgres:18-alpine)"
PORT=""
for _ in $(seq 1 100); do
  PORT="$(docker port "$CONTAINER" 5432/tcp 2>/dev/null | head -1 | sed 's/.*://')"
  [ -n "$PORT" ] && break
  sleep 0.1
done
if [ -z "$PORT" ]; then
  echo "docker did not publish a host port" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
export DW_PG_SERVER="127.0.0.1:$PORT"
export DW_PG_URL="postgres://dropwise:dropwise@127.0.0.1:$PORT/dropwise"
echo "postgres=127.0.0.1:$PORT container=$CONTAINER"

# TCP readiness, not the initdb unix socket: wait for the published loopback
# endpoint, then ask the container's own TCP endpoint (explicit -h) for its
# readiness. No client request is sent before this loop succeeds.
READY=0
for _ in $(seq 1 300); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    if docker exec "$CONTAINER" pg_isready -h 127.0.0.1 -p 5432 -U dropwise -d dropwise \
        >/dev/null 2>&1; then
      READY=1
      break
    fi
  fi
  sleep 0.1
done
if [ "$READY" -ne 1 ]; then
  echo "postgres did not become TCP-ready" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
echo "postgres-ready"

{
  echo "date=$(date -Is)"
  echo "repo=$ROOT"
  echo "container_name=$NAME"
  echo "container=$CONTAINER"
  echo "postgres_endpoint=127.0.0.1:$PORT (loopback only)"
  echo "docker_image=postgres:18-alpine"
  echo "docker_image_id=$(docker image inspect postgres:18-alpine --format '{{.Id}}')"
  echo "docker_repo_digests=$(docker image inspect postgres:18-alpine --format '{{json .RepoDigests}}')"
  echo "server_version=$(docker exec "$CONTAINER" psql -U dropwise -d dropwise -Atc 'show server_version' 2>&1)"
  echo "client=sqlx 0.8.6 (postgres feature, runtime-tokio, direct PgConnection, no pool)"
  echo "sqlx_lock=$(awk '/^name = "sqlx"$/{getline; print}' Cargo.lock | head -1)"
  echo "sqlx_postgres_lock=$(awk '/^name = "sqlx-postgres"$/{getline; print}' Cargo.lock | head -1)"
  echo "tokio_lock=$(awk '/^name = "tokio"$/{getline; print}' Cargo.lock | head -1)"
  echo "tokio_postgres_lock=$(awk '/^name = "tokio-postgres"$/{getline; print}' Cargo.lock | head -1)"
  echo "dropwise=path:$ROOT"
  echo "proxy=dropwise::net::Proxy reply_delay=50ms"
  echo "config=max_runs=64 races=Immediate,AfterWake max_cancellations=1 scenario_timeout=20s settle=0/0 workers=1"
  echo "target=INSERT t1..t3 ON CONFLICT rows_affected=3"
  cargo --version
  rustc --version
  docker --version
} > "$LOGS/environment.txt"

: > "$LOGS/timings.txt"
STATUS=0
run_phase() {
  local name="$1"; shift
  local start; start="$(date +%s)"
  set +e
  "$@" > "$LOGS/$name.stdout.log" 2> "$LOGS/$name.stderr.log"
  local rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  set -e
  echo "$name rc=$rc seconds=$(( $(date +%s) - start ))" >> "$LOGS/timings.txt"
  echo "$name rc=$rc" >&2
  return 0
}

BIN="$HERE/target/release/sqlx-ladder"
export SCAN_VARIANT=sqlx
run_phase preflight "$BIN" preflight
for side in pre post; do
  for rep in 1 2; do
    run_phase "calibrate-$side-$rep" "$BIN" "calibrate-$side"
  done
done
for rep in 1 2 3; do
  run_phase "ladder-$rep" "$BIN" ladder "$rep"
done
# Optional same-protocol contrast: tokio-postgres 0.7.18 direct Client with a
# background connection task, one server-side-applied cut per trial. Not a
# ladder and not counted with the sqlx levels above.
run_phase probe-pg "$HERE/target/release/pg-probe"

echo "done logs=$LOGS"

exit "$STATUS"
