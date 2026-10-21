# Upstream reproductions

Submitted on 2026-10-22:

- psycopg_pool waiter cleanup patch: [PR #1452](https://github.com/psycopg/psycopg/pull/1452).
- psycopg transaction entry cancellation: [issue #1451](https://github.com/psycopg/psycopg/issues/1451), `psycopg_entry.py`.
- asyncpg transaction entry cancellation: [reproduction added to #1386](https://github.com/MagicStack/asyncpg/issues/1386#issuecomment-6328277427), `asyncpg_entry.py`.
- MySQL async query reply mismatch: [issue #112](https://github.com/mysql/mysql-connector-python/issues/112), `mysql_cancel.py`.

The scripts use only the corresponding driver and the standard library. Run
against disposable local databases. The PostgreSQL scripts need CREATE TABLE
permission and take `DATABASE_URL`; the MySQL script uses root and takes
`MYSQL_PORT` and `MYSQL_PASSWORD`.

The transaction scripts demonstrate a successful retry whose insert is not
visible from an independent connection. The MySQL script confirms the first
query is running before cancelling it, then demonstrates its reply being
returned for `SELECT 42`.
