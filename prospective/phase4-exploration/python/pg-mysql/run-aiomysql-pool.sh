#!/usr/bin/env bash
# aiomysql 0.3.2 POOL unit (round 3b): create_pool(minsize=1, maxsize=1,
# autocommit=True) on a disposable MariaDB 11.4, loopback-only ephemeral host
# port.  The direct unit is unchanged and lives in run-aiomysql.sh /
# logs/run.KqRbB4/.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r3b-aio-pool-$$"
DB="dw_r3b_$$"
RUN=$(mktemp -d logs/run.XXXXXX)
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker run -d --rm --name "$NAME" \
  -e MARIADB_ROOT_PASSWORD=dw -e MARIADB_DATABASE="$DB" \
  -p 127.0.0.1::3306 mariadb:11.4 >/dev/null || exit 1
PORT=$(docker port "$NAME" 3306 | sed 's/.*://')
for _ in $(seq 1 90); do
  docker exec "$NAME" mariadb -uroot -pdw -e 'select 1' >/dev/null 2>&1 && break
  sleep 1
done

{
  echo "== aiomysql pool, round 3b =="
  date -Is
  "$PY" -V
  "$PY" - <<'EOF'
import importlib.metadata as m
for p in ("aiomysql", "PyMySQL", "typing-extensions"):
    print(f"{p}=={m.version(p)}")
EOF
  docker image inspect --format 'mariadb:11.4 {{index .RepoDigests 0}}' mariadb:11.4
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  docker exec "$NAME" mariadb -uroot -pdw -N -e 'select version()'
  echo "port=$PORT db=$DB run=$RUN"
} > "$RUN/environment.txt" 2>&1

export PYTHONPATH=".:$REPO/research/python-retro"
"$PY" -m cases.aiomysql_case --port "$PORT" --user root --password dw \
  --dbname "$DB" --mode pool --diag > "$RUN/aiomysql-pool-diag.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/aiomysql-pool-diag.txt"
"$PY" -m cases.aiomysql_case --port "$PORT" --user root --password dw \
  --dbname "$DB" --mode pool --levels B1,B2,B3,M1 --reps 3 \
  > "$RUN/aiomysql-pool.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/aiomysql-pool.txt"
"$PY" -m cases.aiomysql_case --port "$PORT" --user root --password dw \
  --dbname "$DB" --mode pool --probe 1,2,3,4 \
  > "$RUN/aiomysql-pool-probe.txt" 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then STATUS=1; fi
echo "exit=$rc" >> "$RUN/aiomysql-pool-probe.txt"
echo "logs: $RUN"

exit "$STATUS"
