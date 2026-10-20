#!/usr/bin/env bash
# Redis backtracking runs.  Starts one disposable redis:8-alpine container bound
# to 127.0.0.1, runs the two redis sub-cases, removes the container on exit.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
WORK=${DW_PY_WORK:-$HOME/dropwise-py-work}
IMAGE=redis:8-alpine
DIGEST=redis@sha256:9d317178eceac8454a2284a9e6df2466b93c745529947f0cd42a0fa9609d7005
NAME="dw-py-redis-$$"
ROUNDS=${ROUNDS:-3}
TARGETS=${TARGETS:-blpop,pipeline}

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker image inspect "$IMAGE" >/dev/null
observed=$(docker image inspect "$IMAGE" --format '{{.RepoDigests}}' | tr -d '[]')
[[ "$observed" == "$DIGEST" ]] || echo "WARNING: $IMAGE digest changed: $observed" >&2

docker run -d --rm --name "$NAME" -p 127.0.0.1::6379 "$IMAGE" >/dev/null
PORT=$(docker port "$NAME" 6379/tcp | awk -F: '{print $NF}')
for _ in $(seq 1 50); do
    docker exec "$NAME" redis-cli ping >/dev/null 2>&1 && break
    sleep 0.1
done
docker exec "$NAME" redis-cli ping >/dev/null
echo "redis container=$NAME port=127.0.0.1:$PORT image=$DIGEST"
docker exec "$NAME" redis-server --version

run_one() {
    local version=$1 target=$2 level=$3 round=$4
    local log="$ROOT/logs/redis-${target}-${version}-r${round}-${level}.log"
    echo "--- redis $version $target round=$round level=$level -> $log"
    timeout 300 env PYTHONPATH="$ROOT" \
        "$WORK/venvs/redis-$version/bin/python" -m cases.redis_case \
        --url "redis://127.0.0.1:$PORT/0" --target "$target" \
        --levels "$level" --reps 3 --round "$round" \
        >"$log" 2>&1 || echo "EXIT=$? (see $log)" | tee -a "$log"
}

for round in $(seq 1 "$ROUNDS"); do
    # R1: non-pipeline BLPOP, 4.5.3 (incomplete fix) vs 4.5.4 (CVE-2023-28859).
    if [[ ",$TARGETS," == *",blpop,"* ]]; then
        for version in 4.5.3 4.5.4; do
            for level in B1 B2 B3 M1; do
                run_one "$version" blpop "$level" "$round"
            done
        done
    fi
    # R2: pipeline, 4.5.2 vs 4.5.3 (PR #2641 / CVE-2023-28858).
    if [[ ",$TARGETS," == *",pipeline,"* ]]; then
        for version in 4.5.2 4.5.3; do
            for level in B1 B2 B3 M1; do
                run_one "$version" pipeline "$level" "$round"
            done
        done
    fi
done

echo "=== docker ps -a ==="
docker ps -a --format '{{.Names}} {{.Image}} {{.Status}}' | tee "$ROOT/logs/docker-ps-redis.txt"
echo "done"
