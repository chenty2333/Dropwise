#!/usr/bin/env bash
# neo4j 6.4.1 AsyncGraphDatabase pool (max_connection_pool_size=1) on a
# disposable Neo4j community container, loopback-published ports only.
# Unique name, unique log dir, auth neo4j/testpassword (basic, not NONE).
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv-py313/bin/python}
BOLT=${NEO4JBOLT:-57687}
HTTP=${NEO4JHTTP:-57474}
NAME="dw-r3-py-neo4j-$$"
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT

for p in "$BOLT" "$HTTP"; do
  if ss -ltn 2>/dev/null | grep -q ":$p "; then
    echo "port $p already busy" >&2
    exit 1
  fi
done

docker run -d --rm --name "$NAME" \
  -p "127.0.0.1:$BOLT:7687" -p "127.0.0.1:$HTTP:7474" \
  -e NEO4J_AUTH=neo4j/testpassword \
  neo4j:5.26.0-community >/dev/null || exit 1

ready=""
for _ in $(seq 1 120); do
  if docker exec "$NAME" cypher-shell -u neo4j -p testpassword \
       "RETURN 1 AS x" >/dev/null 2>&1; then
    ready=1; break
  fi
  sleep 1
done
if [ -z "$ready" ]; then
  echo "neo4j bolt did not become ready" >&2
  docker logs "$NAME" 2>&1 | tail -40 >&2 || true
  exit 1
fi

{
  echo "== environment =="
  date -u +%FT%TZ
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei '^neo4j|pytz|tenacity' || true
  docker image inspect --format 'neo4j:5.26.0-community {{index .RepoDigests 0}} id={{.Id}}' neo4j:5.26.0-community
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  docker exec "$NAME" cypher-shell -u neo4j -p testpassword --format plain \
    "CALL dbms.components() YIELD name, versions, edition RETURN name, versions[0], edition" 2>/dev/null
  echo "auth=basic neo4j/testpassword (NEO4J_AUTH set; not none)"
  echo "bolt=127.0.0.1:$BOLT http=127.0.0.1:$HTTP delay=0.05 bound=2.0 engine_timeout=12"
  echo "run=$RUN"
} > "$RUN/environment.txt" 2>&1

export PYTHONPATH=".:$REPO/research/python-retro"
for mode in txn plain; do
  "$PY" -m cases.neo4j_case --port "$BOLT" --mode "$mode" \
    --levels B1,B2,B3,M1 --reps 3 > "$RUN/neo4j-$mode.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/neo4j-$mode.txt"
  "$PY" -m cases.neo4j_case --port "$BOLT" --mode "$mode" --diag \
    > "$RUN/neo4j-$mode-diag.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/neo4j-$mode-diag.txt"
done
echo "logs: $RUN"

exit "$STATUS"
