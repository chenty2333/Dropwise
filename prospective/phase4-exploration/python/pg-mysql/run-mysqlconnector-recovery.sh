#!/usr/bin/env bash
# Round 4 recovery contrast for mysql-connector-python 26.7.0 async DIRECT.
#
# One disposable MySQL 9.6 container on a loopback-only ephemeral port, one
# DelayProxy per scenario (50 ms per server->client chunk), the same four
# realised cancellation cuts as round 3b, and the three recovery strategies
# (rollback / reset_session / close+reconnect).  A fresh process is started per
# (cut, strategy) because the cancelled target mutates state; each process runs
# its reps with a fresh scenario (fresh table/proxy/handles).
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r4-mcrec-$$"
DB="dw_r4_mcrec_$$"
RUN=$(mktemp -d logs/run.XXXXXX)
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker run -d --rm --name "$NAME" \
  -e MYSQL_ROOT_PASSWORD=dw -e MYSQL_ROOT_HOST=% -e MYSQL_DATABASE="$DB" \
  -p 127.0.0.1::3306 mysql:9.6 >/dev/null || exit 1
PORT=$(docker port "$NAME" 3306 | sed 's/.*://')
for _ in $(seq 1 90); do
  docker exec "$NAME" mysql -uroot -pdw -e 'select 1' >/dev/null 2>&1 && break
  sleep 1
done

{
  echo "== mysql-connector-python aio direct recovery contrast, round 4 =="
  date -Is
  "$PY" -V
  "$PY" - <<'EOF'
import importlib.metadata as m
print(f"mysql-connector-python=={m.version('mysql-connector-python')}")
EOF
  docker image inspect --format 'mysql:9.6 {{index .RepoDigests 0}}' mysql:9.6
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  docker exec "$NAME" mysql -uroot -pdw -N -e 'select version()' 2>/dev/null
  echo "port=$PORT db=$DB run=$RUN"
} > "$RUN/environment.txt" 2>&1

export PYTHONPATH=".:$REPO/research/python-retro"

# No-service contract regression (fake Scenario; no DB, no proxy).
"$PY" tests/test_adapter_contract.py > "$RUN/adapter-contract-tests.txt" 2>&1
rc=$?; echo "exit=$rc" >> "$RUN/adapter-contract-tests.txt"
[ "$rc" -eq 0 ] || STATUS=1

for k in 1 2 3 4; do
  for s in rollback reset_session close_reconnect; do
    out="$RUN/recovery-cut${k}-${s}.txt"
    "$PY" -m cases.mysqlconnector_recovery_case --port "$PORT" --user root \
      --password dw --dbname "$DB" --cut "$k" --strategy "$s" --reps 3 \
      > "$out" 2>&1
    rc=$?; echo "exit=$rc" >> "$out"
    [ "$rc" -eq 0 ] || STATUS=1
  done
done

echo "logs: $RUN"
exit "$STATUS"
