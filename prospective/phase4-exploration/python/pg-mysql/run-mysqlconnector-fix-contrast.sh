#!/usr/bin/env bash
# Round 4 minimal-fix contrast for mysql-connector-python 26.7.0 async DIRECT.
#
# Sequential on ONE disposable MySQL 9.6 container: original site-packages
# build first, then the patched copy (fix/patched first on PYTHONPATH) which
# closes/poisons the connection when an in-flight command is cancelled.
# Fresh connections per scenario; the container is shared so both builds see
# the same server.  Runs: import check + diag (normal path) + 4-cut probe x3 +
# the 4-level ladder (B1,B2,B3,M1; 3 reps) for each build, plus the recovery
# matrix (4 cuts x 3 strategies x 3 reps) for the patched build.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv313/bin/python}
NAME="dw-r4-mcfix-$$"
DB="dw_r4_mcfix_$$"
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
  echo "== mysql-connector-python aio direct fix contrast, round 4 =="
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

ORIG=".:$REPO/research/python-retro"
PATCHED="$PWD/fix/patched:.:$REPO/research/python-retro"

PYTHONPATH="$PATCHED" "$PY" - > "$RUN/patched-import.txt" 2>&1 <<'EOF'
import mysql.connector
import mysql.connector.aio.abstracts as abstracts
import mysql.connector.aio.network as network
print("mysql.connector.__file__ =", mysql.connector.__file__)
print("aio.network.__file__     =", network.__file__)
print("aio.abstracts.__file__   =", abstracts.__file__)
print("version                  =", mysql.connector.__version__)
assert "/fix/patched/" in mysql.connector.__file__, "patched copy NOT first on sys.path"
assert "_close_on_cancel" in open(network.__file__, encoding="utf-8").read()
print("PATCH ACTIVE")
EOF
rc=$?; echo "exit=$rc" >> "$RUN/patched-import.txt"
[ "$rc" -eq 0 ] || STATUS=1

run_build() {
  build="$1"
  ppath="$2"
  echo "== build=$build PYTHONPATH=$ppath" > "$RUN/${build}-commands.txt"
  PYTHONPATH="$ppath" "$PY" -m cases.mysqlconnector_case --port "$PORT" \
    --user root --password dw --dbname "$DB" --mode direct --diag \
    > "$RUN/${build}-diag.txt" 2>&1
  rc=$?; echo "exit=$rc" >> "$RUN/${build}-diag.txt"
  [ "$rc" -eq 0 ] || STATUS=1
  for rep in 1 2 3; do
    PYTHONPATH="$ppath" "$PY" -m cases.mysqlconnector_case --port "$PORT" \
      --user root --password dw --dbname "$DB" --mode direct --probe 1,2,3,4 \
      > "$RUN/${build}-probe-rep${rep}.txt" 2>&1
    rc=$?; echo "exit=$rc" >> "$RUN/${build}-probe-rep${rep}.txt"
    [ "$rc" -eq 0 ] || STATUS=1
  done
  PYTHONPATH="$ppath" "$PY" -m cases.mysqlconnector_case --port "$PORT" \
    --user root --password dw --dbname "$DB" --mode direct \
    --levels B1,B2,B3,M1 --reps 3 > "$RUN/${build}-ladder.txt" 2>&1
  rc=$?; echo "exit=$rc" >> "$RUN/${build}-ladder.txt"
  [ "$rc" -eq 0 ] || STATUS=1
}

run_build original "$ORIG"
run_build patched "$PATCHED"

# Patched recovery matrix: the same cuts/strategies as the original recovery
# run, to show what an official recovery call does once cancellation poisons
# the handle.
for k in 1 2 3 4; do
  for s in rollback reset_session close_reconnect; do
    out="$RUN/patched-recovery-cut${k}-${s}.txt"
    PYTHONPATH="$PATCHED" "$PY" -m cases.mysqlconnector_recovery_case \
      --port "$PORT" --user root --password dw --dbname "$DB" \
      --cut "$k" --strategy "$s" --reps 3 > "$out" 2>&1
    rc=$?; echo "exit=$rc" >> "$out"
    [ "$rc" -eq 0 ] || STATUS=1
  done
done

echo "logs: $RUN"
exit "$STATUS"
