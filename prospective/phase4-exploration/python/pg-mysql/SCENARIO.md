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

## Round 3b addendum (2026-10-21, post-review)

The two gaps above are now covered, and the adapter contract changed:

* `cases/aiomysql_case.py` (direct **and** pool) and the new
  `cases/mysqlconnector_case.py` return
  `dropwise_py.engine.OpResult(ok=..., stage=..., value=..., error=...)` from
  `same()`/`different()`, stop at the first failed stage (never COMMIT after a
  failed BEGIN/insert), raise for an uncancelled target failure, check
  setup/warm-up, and record close results. The old direct aiomysql B2/B3
  numbers were an adapter false negative; see
  `RESULTS-FRAGMENT-round3b.md` §3.1 for the old-vs-new table.
* aiomysql pool (`create_pool(minsize=1, maxsize=1, autocommit=True)`) is now
  run: `run-aiomysql-pool.sh`, `--mode pool`, canonical log
  `logs/run.GWTJsc/aiomysql-pool.txt`.
* mysql-connector-python async direct is now run:
  `run-mysqlconnector-aio.sh`, `--mode direct`, canonical log
  `logs/run.GWTJsc/mysqlconnector-direct.txt`; internal evidence in
  `.../mysqlconnector-probe.txt`.
* Contract categories (do not change `Expect` globally): (a) pool discards the
  cancelled handle and reconnects = allowed self-heal; (b) direct handle
  refuses reuse with an explicit `InterfaceError` and zero stale effect =
  legitimate fail-fast / contract gap, mechanically counted by B2/B3 but not a
  protocol violation; (c) an operation reports success/rows that do not
  correspond to its own effect = protocol violation (mysql-connector here).
* Canonical final logs are `logs/run.GWTJsc/`; `run.KqRbB4` (old direct) and
  `run.Pio5vb` (pre-fix adapter) are retained but invalid for the corrected
  aiomysql direct B2/B3 numbers. `run.ya23Xb` is an intermediate OpResult run
  whose aiomysql close recording was fixed in `run.GWTJsc`.
* No-service retry-contract regression:
  `tests/test_adapter_contract.py` (OpResult false, raised error, timeout,
  structured timeout, OpResult true).
* Earlier statements in this file ("Pool mode was not run", "mysql-connector
  ... not exercised", "B2/B3 ... wrap per-statement errors") describe the
  pre-review adapter and are superseded by `RESULTS-FRAGMENT-round3b.md`.
