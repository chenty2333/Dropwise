#!/usr/bin/env bash
# pymongo 4.19.0 AsyncMongoClient pool (maxPoolSize=1) on a disposable
# single-node MongoDB replica set (transactions need a replica set).
# Loopback only: host network + --bind_ip 127.0.0.1. Unique name, unique log dir.
set -u
STATUS=0
cd "$(dirname "$0")"
REPO=$(cd ../../../.. && pwd)
PY=${PY:-$HOME/dropwise-py-work-r3/venv-py313/bin/python}
PORT=${MONGOPORT:-57017}
NAME="dw-r3-py-mongo-$$"
RUN=$(mktemp -d logs/run.XXXXXX)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT

if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
  echo "port $PORT already busy" >&2
  exit 1
fi
docker run -d --rm --name "$NAME" --network host mongo:7.0.21 \
  --replSet rs0 --bind_ip 127.0.0.1 --port "$PORT" >/dev/null || exit 1

ready=""
for _ in $(seq 1 60); do
  if docker exec "$NAME" mongosh --quiet --port "$PORT" \
       --eval 'db.runCommand({ping:1}).ok' >/dev/null 2>&1; then
    ready=1; break
  fi
  sleep 0.5
done
if [ -z "$ready" ]; then
  echo "mongod did not become reachable" >&2
  docker logs "$NAME" >&2 || true
  exit 1
fi

docker exec "$NAME" mongosh --quiet --port "$PORT" \
  --eval "rs.initiate({_id:'rs0', members:[{_id:0, host:'127.0.0.1:$PORT'}]})" \
  >/dev/null 2>&1

P=""
for _ in $(seq 1 60); do
  P=$(docker exec "$NAME" mongosh --quiet --port "$PORT" \
        --eval 'db.hello().isWritablePrimary' 2>/dev/null)
  [ "$P" = "true" ] && break
  sleep 0.5
done
if [ "$P" != "true" ]; then
  echo "replica set has no primary" >&2
  exit 1
fi

{
  echo "== environment =="
  date -u +%FT%TZ
  "$PY" -V
  "$PY" -m pip freeze 2>/dev/null | grep -Ei 'pymongo|dnspython' || true
  docker image inspect --format 'mongo:7.0.21 {{index .RepoDigests 0}} id={{.Id}}' mongo:7.0.21
  docker inspect --format 'container={{.Name}} image={{.Config.Image}} id={{.Image}}' "$NAME"
  docker exec "$NAME" mongosh --quiet --port "$PORT" \
    --eval 'JSON.stringify(db.hello())' 2>/dev/null | head -c 400
  echo
  docker exec "$NAME" mongosh --quiet --port "$PORT" --eval 'db.version()' 2>/dev/null
  echo "replSet=rs0 single-node, mongod --bind_ip 127.0.0.1 --port $PORT (host network)"
  echo "port=$PORT delay=0.05 bound=2.0 engine_timeout=12 python=3.13"
  echo "run=$RUN"
} > "$RUN/environment.txt" 2>&1

export PYTHONPATH=".:$REPO/research/python-retro"
for mode in txn plain; do
  "$PY" -m cases.mongo_case --port "$PORT" --mode "$mode" \
    --levels B1,B2,B3,M1 --reps 3 > "$RUN/mongo-$mode.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/mongo-$mode.txt"
  "$PY" -m cases.mongo_case --port "$PORT" --mode "$mode" --diag \
    > "$RUN/mongo-$mode-diag.txt" 2>&1
  rc=$?
  if [ "$rc" -ne 0 ]; then STATUS=1; fi
  echo "exit=$rc" >> "$RUN/mongo-$mode-diag.txt"
done
echo "logs: $RUN"

exit "$STATUS"
