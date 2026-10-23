#!/usr/bin/env bash
# mysql-connector-python 26.7.0 (mysql.connector.aio) DIRECT unit, round 3b:
# one long-lived async connection on a disposable MySQL 9.6, loopback-only
# ephemeral host port.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$REPO/.venv/bin/python}
NAME="dw-r3b-mc-aio-$$"
DB="dw_r3b_$$"
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
  echo "== mysql-connector-python aio direct, round 3b =="
  date -Is
  "$PY" -V
  "$PY" - <<'EOF'
import importlib.metadata as m
print(f"mysql-connector-python=={m.version('mysql-connector-python')}")
print(f"typing-extensions=={m.version('typing-extensions')}")
EOF
  docker image inspect --format 'mysql:9.6 {{index .RepoDigests 0}}' mysql:9.6
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  docker exec "$NAME" mysql -uroot -pdw -N -e 'select version()' 2>/dev/null
  echo "port=$PORT db=$DB run=$RUN"
} > "$RUN/environment.txt" 2>&1

export PYTHONPATH=".:$REPO/research/python-retro"
"$PY" -m cases.mysqlconnector_case --port "$PORT" --user root --password dw \
  --dbname "$DB" --mode direct --diag > "$RUN/mysqlconnector-diag.txt" 2>&1
"$PY" -m cases.mysqlconnector_case --port "$PORT" --user root --password dw \
  --dbname "$DB" --mode direct --levels B1,B2,B3,M1 --reps 3 \
  > "$RUN/mysqlconnector-direct.txt" 2>&1
"$PY" -m cases.mysqlconnector_case --port "$PORT" --user root --password dw \
  --dbname "$DB" --mode direct --probe 1,2,3,4 \
  > "$RUN/mysqlconnector-probe.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/mysqlconnector-direct.txt"
echo "logs: $RUN"

exit "$STATUS"
