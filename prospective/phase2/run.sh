#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
MANIFEST="$SCRIPT_DIR/Cargo.toml"
RUNS="$SCRIPT_DIR/runs"
IMAGE='nats:2.11.6-alpine'
CONTAINER="dropwise-phase2-nats-$(date +%s)-$$"
container_id=''
failures=0
suite_started_utc=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
suite_started_epoch=$(date +%s)

cleanup() {
    if [[ -n "$container_id" ]]; then
        docker logs "$container_id" >"$RUNS/logs/service/server.log" 2>&1 || true
        docker stop "$container_id" >/dev/null 2>&1 || true
        container_id=''
    fi
}
trap cleanup EXIT INT TERM

mkdir -p "$RUNS/logs/service"
if ! container_id=$(docker run --rm -d --name "$CONTAINER" -p 127.0.0.1::4222 "$IMAGE" -js); then
    echo "could not start the temporary NATS service" >&2
    exit 1
fi

ready=0
for _ in $(seq 1 100); do
    if docker logs "$container_id" 2>&1 | grep -q 'Server is ready'; then
        ready=1
        break
    fi
    sleep 0.1
done
if [[ "$ready" != 1 ]]; then
    docker logs "$container_id" >"$RUNS/logs/service/startup-failure.log" 2>&1 || true
    cat "$RUNS/logs/service/startup-failure.log"
    echo "NATS did not become ready; no scenarios were run" >&2
    exit 1
fi

port=$(docker port "$container_id" 4222/tcp | awk -F: '/127\.0\.0\.1/{print $NF; exit}')
if [[ -z "$port" ]]; then
    echo "could not determine loopback NATS port" >&2
    exit 1
fi
NATS_URL="nats://127.0.0.1:$port"
printf 'image=%s\nurl=%s\n' "$IMAGE" "$NATS_URL" | tee "$RUNS/logs/service/ready.txt"

run_one() {
    local scene="$1" test_name="$2" mode="$3" repetition="$4"
    local log="$RUNS/logs/$scene/$mode/$repetition.log"
    mkdir -p "$(dirname "$log")"
    echo "=== $scene mode=$mode RUN=$repetition ==="
    if PHASE2_MODE="$mode" RUN="$repetition" NATS_URL="$NATS_URL" \
        cargo test --locked --manifest-path "$MANIFEST" --test scenarios "$test_name" -- --exact --nocapture \
        >"$log" 2>&1; then
        cat "$log"
        echo "exit_code=0" >>"$log"
    else
        local status=$?
        cat "$log"
        echo "exit_code=$status" >>"$log"
        failures=$((failures + 1))
    fi
}

run_group() {
    local mode="$1"
    run_one moka_leader_cancel moka_leader_cancel "$mode" 1
    run_one moka_leader_cancel moka_leader_cancel "$mode" 2
    run_one moka_leader_cancel moka_leader_cancel "$mode" 3
    run_one moka_waiter_cancel moka_waiter_cancel "$mode" 1
    run_one moka_waiter_cancel moka_waiter_cancel "$mode" 2
    run_one moka_waiter_cancel moka_waiter_cancel "$mode" 3
    run_one nats_stream_resume nats_stream_resume "$mode" 1
    run_one nats_stream_resume nats_stream_resume "$mode" 2
    run_one nats_stream_resume nats_stream_resume "$mode" 3
    run_one nats_process_cancel nats_process_cancel "$mode" 1
    run_one nats_process_cancel nats_process_cancel "$mode" 2
    run_one nats_process_cancel nats_process_cancel "$mode" 3
}

# Verify every uncancelled baseline before any injected run.
run_group baseline
if [[ "$failures" -ne 0 ]]; then
    echo "Baseline failure: stopping before cancellation injection" >&2
    exit 1
fi

run_group simple
run_group dropwise-matched
run_group dropwise-full

docker logs "$container_id" >"$RUNS/logs/service/server.log" 2>&1 || true
if [[ "$failures" -ne 0 ]]; then
    echo "$failures scenario invocation(s) failed; see $RUNS/logs and reports" >&2
    exit 1
fi
suite_finished_utc=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
suite_elapsed_seconds=$(( $(date +%s) - suite_started_epoch ))
printf 'started_utc=%s\nfinished_utc=%s\nwall_seconds=%s\nimage=%s\n' \
    "$suite_started_utc" "$suite_finished_utc" "$suite_elapsed_seconds" "$IMAGE" \
    >"$RUNS/logs/suite-summary.txt"
cat "$RUNS/logs/suite-summary.txt"
echo "all phase2 runs completed successfully"
