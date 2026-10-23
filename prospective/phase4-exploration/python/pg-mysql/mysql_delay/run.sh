#!/usr/bin/env bash
# Round-5 MySQL delay/re-cancellation matrix (mysql-connector-python 26.7.0 aio).
#
# Uses the existing real MySQL server on 127.0.0.1:32922, creates one unique
# database for this run, and drops it at the end.  Original build = the copy in
# ~/dropwise-py-work-r3/venv313; patched build = fix/patched first on
# PYTHONPATH.  Every case invocation is a fresh process.  No global server
# variable is changed and no existing file is touched.
set -u
cd "$(dirname "$0")/.."          # prospective/phase4-exploration/python/pg-mysql
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$REPO/.venv/bin/python}
HOST=${HOST:-127.0.0.1}
PORT=${PORT:-32922}
DB=${DB:-dw_r5_mysqldelay_$$}
RUN=$(mktemp -d mysql_delay/logs/run.XXXXXX)
STATUS=0
export PYTHONUNBUFFERED=1

cleanup() {
  "$PY" - "$HOST" "$PORT" "$DB" >/dev/null 2>&1 <<'EOF'
import sys, mysql.connector
host, port, db = sys.argv[1], int(sys.argv[2]), sys.argv[3]
try:
    c = mysql.connector.connect(host=host, port=port, user="root",
                                password="dw", connection_timeout=10)
    cur = c.cursor()
    cur.execute(f"DROP DATABASE IF EXISTS `{db}`")
    c.close()
except BaseException as exc:
    print(f"drop db failed: {exc!r}")
EOF
}
trap cleanup EXIT

# Unique database for this run only.
"$PY" - "$HOST" "$PORT" "$DB" <<'EOF'
import sys, mysql.connector
host, port, db = sys.argv[1], int(sys.argv[2]), sys.argv[3]
c = mysql.connector.connect(host=host, port=port, user="root",
                            password="dw", connection_timeout=10)
cur = c.cursor()
cur.execute(f"CREATE DATABASE IF NOT EXISTS `{db}`")
cur.execute(f"USE `{db}`")
cur.execute("CREATE TABLE IF NOT EXISTS _dw_probe (k INT PRIMARY KEY)")
cur.execute("DROP TABLE _dw_probe")
c.close()
print(f"created database {db}")
EOF

ORIG=".:$REPO/research/python-retro"
PATCHED="$PWD/fix/patched:.:$REPO/research/python-retro"

{
  echo "== mysql-connector-python aio recovery delay / re-cancel, round 5 =="
  date -Is
  echo "host=$HOST port=$PORT db=$DB run=$RUN"
  echo "repo=$REPO"
  echo "git_head=$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
  "$PY" -V
  "$PY" - <<'EOF'
import importlib.metadata as m, sys
print(f"python={sys.version.split()[0]}")
print(f"mysql-connector-python=={m.version('mysql-connector-python')}")
import mysql.connector
print(f"connector_file={mysql.connector.__file__}")
EOF
  uname -a
  "$PY" - "$HOST" "$PORT" <<'EOF'
import sys, mysql.connector
host, port = sys.argv[1], int(sys.argv[2])
c = mysql.connector.connect(host=host, port=port, user="root",
                            password="dw", connection_timeout=10)
cur = c.cursor()
cur.execute("SELECT VERSION(), @@port, @@log_output, @@general_log, "
            "@@performance_schema, @@transaction_isolation")
print("server=" + repr(cur.fetchall()))
cur.execute("SELECT NAME, ENABLED FROM performance_schema.setup_consumers "
            "WHERE NAME IN ('events_transactions_current', "
            "'events_transactions_history', 'events_statements_history')")
print("consumers=" + repr(cur.fetchall()))
c.close()
EOF
} > "$RUN/environment.txt" 2>&1

# --- case runner ----------------------------------------------------------
CASE_TIMEOUT=${CASE_TIMEOUT:-300}
run_case() {
  local name="$1" build="$2" target="$3" delay="$4" strategy="$5"
  local mode="$6" reps="$7" escalate="${8:-}"
  local pp="$ORIG" extra=""
  [ "$build" = "patched" ] && pp="$PATCHED"
  [ -n "$escalate" ] && extra="--escalate"
  local cmd=("$PY" -m mysql_delay.case --host "$HOST" --port "$PORT"
             --user root --password dw --dbname "$DB" --build "$build"
             --target "$target" --delay "$delay" --strategy "$strategy"
             --mode "$mode" --reps "$reps" --jsonl "$RUN/records.jsonl")
  [ -n "$extra" ] && cmd+=("$extra")
  printf 'PYTHONPATH=%s timeout %s %q ' "$pp" "$CASE_TIMEOUT" "${cmd[0]}" \
    >> "$RUN/commands.txt"
  printf '%q ' "${cmd[@]:1}" >> "$RUN/commands.txt"
  printf '\n' >> "$RUN/commands.txt"
  echo "[$(date +%H:%M:%S)] START $name" >&2
  PYTHONPATH="$pp" timeout "$CASE_TIMEOUT" "${cmd[@]}" > "$RUN/$name.txt" 2>&1
  local rc=$?
  echo "exit=$rc" >> "$RUN/$name.txt"
  if [ "$rc" -ne 0 ]; then
    STATUS=1
    echo "[$(date +%H:%M:%S)] FAIL  $name exit=$rc" >&2
  else
    echo "[$(date +%H:%M:%S)] done  $name" >&2
  fi
}

# --- short-delay full factorial (2 reps) ---------------------------------
for build in original patched; do
  for target in begin savepoint; do
    for strategy in rollback reset_session close_reconnect; do
      for mode in single recancel; do
        run_case "short-${build}-${target}-${strategy}-${mode}" \
          "$build" "$target" 0.05 "$strategy" "$mode" 2
      done
    done
  done
done
# Nested-specific legitimate recovery (ROLLBACK TO SAVEPOINT; COMMIT outer).
for build in original patched; do
  run_case "short-${build}-savepoint-savepoint_rollback_commit-single" \
    "$build" savepoint 0.05 savepoint_rollback_commit single 2
done

# --- long-delay cells (5 s per server->client chunk, 1 rep) --------------
# Top-level BEGIN target.
for build in original patched; do
  for strategy in rollback reset_session close_reconnect; do
    run_case "long5-${build}-begin-${strategy}-single" \
      "$build" begin 5.0 "$strategy" single 1
    run_case "long5-${build}-begin-${strategy}-recancel" \
      "$build" begin 5.0 "$strategy" recancel 1
  done
done
# Legitimate nested SAVEPOINT target.
for build in original patched; do
  for strategy in rollback close_reconnect; do
    run_case "long5-${build}-savepoint-${strategy}-single" \
      "$build" savepoint 5.0 "$strategy" single 1
    run_case "long5-${build}-savepoint-${strategy}-recancel" \
      "$build" savepoint 5.0 "$strategy" recancel 1
  done
  run_case "long5-${build}-savepoint-savepoint_rollback_commit-single" \
    "$build" savepoint 5.0 savepoint_rollback_commit single 1
done

# One explicit escalation measurement for the direct-close fallback after a
# cancelled reconnect (original build only: the patched build is already
# evicted at cancel).
run_case "long5-original-begin-close_reconnect-recancel-escalate" \
  original begin 5.0 close_reconnect recancel 1 --escalate

# --- analysis -------------------------------------------------------------
"$PY" mysql_delay/analyze.py "$RUN/records.jsonl" > "$RUN/summary.txt" 2>&1
rc=$?; echo "exit=$rc" >> "$RUN/summary.txt"
[ "$rc" -eq 0 ] || STATUS=1

echo "run=$RUN"
echo "records=$(wc -l < "$RUN/records.jsonl" 2>/dev/null || echo 0)"
exit "$STATUS"
