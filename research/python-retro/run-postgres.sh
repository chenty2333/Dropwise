#!/usr/bin/env bash
# SQLAlchemy + asyncpg backtracking runs.  Starts one disposable
# postgres:18-alpine container bound to 127.0.0.1 and runs:
#   * timeout-begin: the issue's own fault injection at BEGIN (1.4.17/1.4.18)
#   * ladder: Task.cancel() at a subset of suspension points (19..23)
# The container is removed on exit.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
WORK=${DW_PY_WORK:-$HOME/dropwise-py-work}
IMAGE=postgres:18-alpine
DIGEST=postgres@sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa
NAME="dw-py-pg-$$"
ROUNDS=${ROUNDS:-3}
MODES=${MODES:-timeout-begin,ladder}
PLANS=${PLANS:-2,3,4,5,6}

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker image inspect "$IMAGE" >/dev/null
observed=$(docker image inspect "$IMAGE" --format '{{.RepoDigests}}' | tr -d '[]')
[[ "$observed" == "$DIGEST" ]] || echo "WARNING: $IMAGE digest changed: $observed" >&2

docker run -d --rm --name "$NAME" \
    -e POSTGRES_USER=dropwise -e POSTGRES_PASSWORD=dropwise -e POSTGRES_DB=dropwise \
    -p 127.0.0.1::5432 "$IMAGE" >/dev/null
PORT=$(docker port "$NAME" 5432/tcp | awk -F: '{print $NF}')
ready=0
for _ in $(seq 1 600); do
    if docker exec "$NAME" psql -U dropwise -d dropwise -tAc 'select 1' >/dev/null 2>&1; then
        # The entrypoint starts a temporary server during init and then
        # restarts; require a second successful query after a short pause.
        sleep 0.5
        if docker exec "$NAME" psql -U dropwise -d dropwise -tAc 'select 1' >/dev/null 2>&1; then
            ready=1
            break
        fi
    fi
    sleep 0.1
done
if [[ "$ready" != 1 ]]; then
    echo "ERROR: postgres did not become ready" >&2
    docker logs "$NAME" 2>&1 | tail -40 >&2
    exit 1
fi
docker exec "$NAME" psql -U dropwise -d dropwise -tAc 'select 1' >/dev/null
URL="postgresql+asyncpg://dropwise:dropwise@127.0.0.1:$PORT/dropwise"
echo "postgres container=$NAME port=127.0.0.1:$PORT image=$DIGEST"
docker exec "$NAME" psql -U dropwise -d dropwise -tAc 'select version();'

clean_sessions() {
    docker exec "$NAME" psql -U dropwise -d dropwise -tAc \
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename='dropwise' AND pid <> pg_backend_pid();" \
        >/dev/null 2>&1 || true
}

run_one() {
    local version=$1 mode=$2 level=$3 round=$4 plans=$5
    local tag="$mode"
    local log="$ROOT/logs/sqla-${tag}-${version}-r${round}-${level}.log"
    clean_sessions
    echo "--- sqlalchemy $version $mode round=$round level=$level -> $log"
    timeout 240 env PYTHONPATH="$ROOT" \
        "$WORK/venvs/sqlalchemy-$version/bin/python" -m cases.sqla_case \
        --url "$URL" --mode "$mode" --plans "$plans" \
        --levels "$level" --reps 3 --round "$round" \
        >"$log" 2>&1 || echo "EXIT=$? (see $log)" | tee -a "$log"
}

for round in $(seq 1 "$ROUNDS"); do
    if [[ ",$MODES," == *",timeout-begin,"* ]]; then
        # The issue's actual path: asyncpg TimeoutError while BEGIN is issued.
        for version in 1.4.17 1.4.18; do
            for level in B1 B2 B3 M1; do
                run_one "$version" timeout-begin "$level" "$round" "$PLANS"
            done
        done
    fi
    if [[ ",$MODES," == *",ladder,"* ]]; then
        # Pure Task.cancel() ladder; only the plans that do not deadlock the
        # greenlet bridge are enumerated (see RESULTS section 8).
        for version in 1.4.17 1.4.18; do
            for level in B1 B2 B3 M1; do
                run_one "$version" ladder "$level" "$round" "$PLANS"
            done
        done
    fi
done

echo "=== docker ps -a ==="
docker ps -a --format '{{.Names}} {{.Image}} {{.Status}}' | tee "$ROOT/logs/docker-ps-postgres.txt"
echo "done"
