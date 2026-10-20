#!/usr/bin/env bash
# aiohttp backtracking runs.  No container: the server is an in-process
# aiohttp.web app.  Runs the four levels in both 3.14.0 and 3.14.1, plus the
# idle-timeout diagnostic that reproduces the issue's real ~2 ms signature.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
WORK=${DW_PY_WORK:-$HOME/dropwise-py-work}
ROUNDS=${ROUNDS:-3}

run_one() {
    local version=$1 level=$2 round=$3
    local log="$ROOT/logs/aiohttp-${version}-r${round}-${level}.log"
    echo "--- aiohttp $version round=$round level=$level -> $log"
    timeout 180 env PYTHONPATH="$ROOT" \
        "$WORK/venvs/aiohttp-$version/bin/python" -m cases.aiohttp_case \
        --mode ladder --levels "$level" --reps 3 --round "$round" --instrument \
        >"$log" 2>&1 || echo "EXIT=$? (see $log)" | tee -a "$log"
}

for round in $(seq 1 "$ROUNDS"); do
    for version in 3.14.0 3.14.1; do
        for level in B1 B2 B3 M1; do
            run_one "$version" "$level" "$round"
        done
        idle_log="$ROOT/logs/aiohttp-${version}-r${round}-idle.log"
        timeout 60 env PYTHONPATH="$ROOT" \
            "$WORK/venvs/aiohttp-$version/bin/python" -m cases.aiohttp_case \
            --mode idle --round "$round" >"$idle_log" 2>&1 || true
        echo "--- aiohttp $version round=$round idle -> $idle_log"
    done
done

echo "done"
