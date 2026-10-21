#!/usr/bin/env bash
# aiomysql 0.3.2 DIRECT unit on MariaDB 11.4.  Pool mode lives in
# run-aiomysql-pool.sh (round 3b) and is reported in RESULTS-FRAGMENT-round3b.md.
set -u
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r3-py-my-$$"
PORT=${MYPORT:-53306}
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
export PYTHONPATH=".:$REPO/research/python-retro"

docker run -d --rm --name "$NAME" \
  -e MARIADB_ROOT_PASSWORD=dw -e MARIADB_DATABASE=dw \
  -p "127.0.0.1:$PORT:3306" mariadb:11.4 >/dev/null || exit 1
for _ in $(seq 1 90); do
  docker exec "$NAME" mariadb -uroot -pdw -e 'select 1' >/dev/null 2>&1 && break
  sleep 1
done

{
  echo "== environment =="
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'aiomysql|pymysql' || true
  docker image inspect --format 'mariadb:11.4 {{index .RepoDigests 0}}' mariadb:11.4
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  echo "port=$PORT run=$RUN"
} > "$RUN/environment.txt" 2>&1

"$PY" -m cases.aiomysql_case --port "$PORT" --user root --password dw \
  --mode direct --levels B1,B2,B3,M1 --reps 3 > "$RUN/aiomysql-direct.txt" 2>&1
echo "exit=$?" >> "$RUN/aiomysql-direct.txt"
echo "logs: $RUN"
