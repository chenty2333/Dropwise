# SCENARIO — psycopg / asyncpg / aiomysql / mysql-connector-python (round 3)

Unfrozen exploration. One `Scenario` subclass per target/mode, a fresh instance per
run; engine and delay proxy are imported read-only from `research/python-retro/`.

## Pending mechanism (all targets, all levels)

`DelayProxy` from `research/python-retro/cases/common.py`, 50 ms on every
server→client chunk. It wraps only the tested connection; the observer connects
directly to the server. No server-side `SLEEP`, no fixed settle, no second
mechanism.

## psycopg 3.3.6 + psycopg-pool 3.3.3

* Reuse unit pool: `AsyncConnectionPool(min_size=1, max_size=1)`, autocommit via
  `configure`; target = `getconn()` → `BEGIN` → upsert row `target` →
  `COMMIT` → `putconn()` (three awaits in one target coroutine).
* Reuse unit direct: one `AsyncConnection(autocommit=True)` created in setup and
  borrowed by the target; no close/reset/reconnect between target and survivor.
* Survivor (both): same pool/handle, upsert row `survivor`, same-connection
  readback, record `conn.info.transaction_status` at acquire.
* Observer: separate direct `AsyncConnection`, `SELECT k,v ORDER BY k`; never
  through the pool or proxy.
* Contract under test: an abandoned transaction must not make the next
  user's committed work invisible. `Expect.ATOMIC`; pre = survivor only,
  post = survivor + target.

## asyncpg 0.32.0

* pool: `create_pool(min_size=1, max_size=1)`, `acquire()` / `release()`.
* direct: one `asyncpg.connect()` connection, same borrowing rule.
* Target/survivor/observer: same shape as psycopg, with `REPLACE`-free
  `INSERT ... ON CONFLICT`; state field = `conn.is_in_transaction()` at
  survivor acquire. `execute()` outside a transaction block is autocommit at
  the server, so a survivor write inside a leftover block is uncommitted.
* Diagnostic (not judged): `pg_stat_activity.state/backend_xid/query` for the
  tested backend pid, and explicit `ROLLBACK` recovery for plans #2/#3.

## aiomysql 0.3.2 (direct only in this report)

* One `aiomysql.connect(autocommit=True)` handle; target = `BEGIN` →
  `REPLACE target` → `COMMIT`; survivor = `REPLACE survivor` + readback;
  observer = independent direct aiomysql connection; state field =
  `Connection.get_transaction_status()` at acquire.
* MariaDB 11.4 (`root`/password, no auth-plugin issue).
* Pool mode was not run inside the timebox.

## mysql-connector-python 26.7.0

Installed in the venv, not exercised: the timebox was spent on the three
PostgreSQL/MariaDB units above. No claim is made about it.

## Levels

`level_b1`, `level_retry(level="B2", retry="same")`,
`level_retry(level="B3", retry="different")`,
`explore_observed(Expect.ATOMIC, reps=3)`. Every operation inside the scenario
is bounded with `asyncio.wait_for(..., 2 s)`; the engine timeout (12 s) is only
a backstop so a hang becomes a recorded value instead of killing the run.
`CancelledError` is re-raised, never swallowed. `close()` is bounded and runs
after survivor/observer.

## Known limits

* Python 3.13.13 is required: on 3.14 the retro engine's pure-Python `Task`
  does not satisfy `asyncio.timeout`'s `current_task()` check, so a nested
  `wait_for` inside the target raises `RuntimeError: Timeout should be used
  inside a task` and yields stay 0. This is an engine/runtime limit, not a
  finding about any client.
* B2/B3 only judge an escaping exception. The scenarios' `same`/`different`
  wrap per-statement errors into return values, so B2/B3 can report "ok" while
  the retried transaction was semantically corrupted (psycopg direct shows
  this). B2/B3 values are in the logs but do not drive the verdict.
