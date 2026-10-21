#!/usr/bin/env bash
# mysql-connector-python 26.7.0 (mysql.connector.aio) was installed in the
# shared venv but NOT exercised: the round-3 timebox went to psycopg, asyncpg
# and aiomysql.  This script is a placeholder so the gap is visible; it starts
# a disposable mysql:9.6 and stops there.
set -u
cd "$(dirname "$0")"
NAME="dw-r3-py-mc-$$"
PORT=${MCPORT:-53307}
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
docker run -d --rm --name "$NAME" \
  -e MYSQL_ROOT_PASSWORD=dw -e MYSQL_DATABASE=dw \
  -p "127.0.0.1:$PORT:3306" mysql:9.6 >/dev/null || exit 1
for _ in $(seq 1 90); do
  docker exec "$NAME" mysql -uroot -pdw -e 'select 1' >/dev/null 2>&1 && break
  sleep 1
done
echo "mysql:9.6 is up on 127.0.0.1:$PORT; no mysql-connector case was written or run."
