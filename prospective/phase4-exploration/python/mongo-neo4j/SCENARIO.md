# SCENARIO — pymongo 4.19.0 / neo4j 6.4.1 (round 3, Python pool units)

Unfrozen exploration (2026-10-21). Not Phase 4 data, no preregistration, no
commits. One `Scenario` subclass per package; a fresh instance per run (fresh
collection/label, fresh client/driver, fresh proxy). Engine and delay proxy are
imported read-only from `research/python-retro/`.

## Pending mechanism (all targets, all levels)

`DelayProxy` from `research/python-retro/cases/common.py`, **50 ms on every
forwarded chunk in both directions** (the same helper and delay as the
`pg-mysql` batch). It wraps only the tested client/driver; the observer
connects directly to the server, never through the proxy. No server-side sleep,
no fixed settle, no second mechanism.

## Services

* **MongoDB 7.0.21**, loopback-only single-node replica set `rs0`
  (`--network host --replSet rs0 --bind_ip 127.0.0.1 --port <ephemeral>`,
  `rs.initiate` with `127.0.0.1:<port>`, wait for `isWritablePrimary`). A
  replica set is required because MongoDB transactions need one; the driver
  connects with `directConnection=true` through the proxy port.
* **Neo4j 5.26.0 community**, loopback-published ports (Bolt 7687→ephemeral,
  HTTP 7474→ephemeral), auth `neo4j/testpassword` (basic; `NEO4J_AUTH` not
  `none`). Bolt only; no routing (`bolt://`, not `neo4j://`).

Both are disposable containers removed by the runner's `EXIT` trap.

## Reuse units

### pymongo 4.19.0 — `AsyncMongoClient(maxPoolSize=1)`

`ClientSession` is **not** a physical connection handle. The checkout is per
operation; with `maxPoolSize=1` a leaked or pinned connection makes the next
operation block, which is what the survivor observes.

| unit | target (state-establishing roundtrip) | B2 same | B3 different | survivor (same client) | observer |
|---|---|---|---|---|---|
| `txn` | `start_session()` → **await** `start_transaction()` (first network roundtrip) → `replace_one(target)` with the session → `commit_transaction()` | same transaction, same `target` doc/value | plain `replace_one(b3)` outside a transaction (different operation) | `replace_one(survivor)` + same-client `find_one` readback | independent `AsyncMongoClient`, direct, reads all docs of the collection |
| `plain` | one ordinary `replace_one(target, upsert)` (implicit session, complete command/reply) | same `replace_one(target)` | `update_one(b3, $set, upsert)` (different write op) | `replace_one(survivor)` + `find_one` readback | same |

Target cleanup (the documented explicit form, executed in `finally`):
`abort_transaction()` if still in a transaction, then `end_session()`. The
state **before** cleanup is recorded in `diag` (`target_final`): transaction
state (`NONE/STARTING/IN_PROGRESS/COMMITTED/COMMITTED_EMPTY/ABORTED`),
`in_transaction`, `has_ended`, and pool counters. The transaction is a real
server transaction (`start_transaction` + commit), never simulated.

### neo4j 6.4.1 — `AsyncGraphDatabase.driver(max_connection_pool_size=1)`

The high-level `AsyncSession` is not a fixed physical handle; closing it
returns/discards the Bolt connection. `AsyncSession.execute_query` does not
exist in 6.4.1 (verified by introspection; only `execute_read`/`execute_write`/
`run`), so the explicit-transaction path is `begin_transaction()` + `run`.

| unit | target | B2 same | B3 different | survivor (same driver) | observer |
|---|---|---|---|---|---|
| `txn` | `session.begin_transaction()` → `tx.run(MERGE target) RETURN` → `single()` → `tx.commit()` | same MERGE, same key/value | ordinary `session.run(MERGE b3)` (autocommit, different operation) | same driver: `session.run(MERGE survivor)` + same-session `MATCH ... RETURN n.v` readback | independent `AsyncDriver`, direct, `MATCH (n:Label) RETURN k,v ORDER BY k` |
| `plain` | `session.run(MERGE target) RETURN` + `single()` + full `consume()` | same | `CREATE (b3)` (different write op) | same | same |

Target cleanup: `tx.rollback()` if not `tx.closed()`, then `session.close()`.
`diag` records `tx.closed()` before/after cleanup, session close result, and the
pool's `in_use`/`idle` counts plus connection identities, so "returned to the
pool" vs "discarded and re-opened" is distinguishable.

**Label/collection isolation**: every scenario instance uses a unique
collection name / node label, so runs never share data; observations are
sanitised (`<name>`, `127.0.0.1:<port>`) before they enter the judged value.

## Judged observation (M1, `Expect.ATOMIC`)

`survivor` = concrete return values of the same-client survivor write and
same-client readback, in a structured
`dropwise_py.engine.OpResult(ok, stage, value, error)`; `observer` = independent
client's documents/nodes (also `OpResult`). A failed survivor stage therefore
cannot compare equal to pre/post (`ok=False` -> `Neither` -> violation).
Pool/session state at acquire is recorded in `diag` but **not judged**: a
cancelled command may legitimately make the driver discard the socket and
re-open one for the survivor (legal self-heal), while the committed data is
still a clean `Post`. A genuinely leaked sole connection is not silent — it
makes the survivor operation hit its 2 s bound (`ok=False`, `stage` names the
failing write/readback).

pre = survivor only (no target); post = target completed + survivor; two
samples per side. `Expect.ATOMIC` allows Pre or Post; `Neither`/`Unknown` are
reported as violations/unknown. pre==post would be a degenerate observation
and is recorded as such (neither package produced it).

## Levels

`level_b1`, `level_retry(level="B2", retry="same")`,
`level_retry(level="B3", retry="different")`,
`explore_observed(Expect.ATOMIC, reps=3)`. Every operation inside a scenario is
bounded with `asyncio.wait_for(..., 2 s)`; the engine timeout (12 s) is only a
backstop. `CancelledError` is re-raised, never swallowed. `close()` is bounded
and runs after survivor/observer.

B1 violation = escaping error or timeout. B2/B3 return `OpResult` and the
engine's `operation_failed()` checks the structured `ok`; a retry with any
failed stage (`err` or 2 s `timeout`) is a violation, and a raising retry is
caught by the engine as `ERR ...`. `target()` raises if an **uncancelled**
target completed with a failed stage, so a broken baseline cannot silently make
`post` equal `pre`. Cleanup is bounded and recorded (`diag`, and
`value["cleanup"]` on `OpResult`), never silent. `cases/test_structural.py`
(no service) asserts the failed-retry, timed-out-retry, raising-target and
clean-retry paths. M1 judges the observation against pre/post.

## Contract under test

* **MongoDB**: a transaction's writes become visible atomically at commit;
  before commit they are invisible. A cancelled commit may be reported as
  `UnknownTransactionCommitResult` and retried, so the allowed outcomes after a
  cancellation are "target doc absent" or "target doc present", nothing else;
  the abandoned session/transaction must not make the next operation on the
  same client fail or hang. The next operation is an ordinary write, not a
  transaction replay.
* **Neo4j**: a transaction commits atomically or rolls back; closing the
  session returns the connection to the pool (or discards it) so the next
  session can acquire a connection. With `max_connection_pool_size=1`, a
  connection that is neither returned nor discarded blocks the next session
  (`connection_acquisition_timeout`), which is a user-visible failure.
* Both are checked with public documentation statements (driver docstrings /
  transaction docs) and the independent observer, not with `Ok`.

## Known limits / coverage

* Only outer `await`s of the target coroutine are cancellation points. Driver
  internals that are not awaited by the task (pymongo's background topology
  monitor, socket reader callbacks) are **not** individually cancellable; the
  ladder covers task-step suspensions only.
* One cancellation per run (`max_cancellations = 1`), reps 3, no concurrent
  operations, no TLS, no server-side failpoints, single server version each,
  pool size 1 only. B2/B3 are one retry, not a recovery policy.
* `AsyncClientSession.start_session()` is synchronous in pymongo 4.19.0, so it
  is not a yield; the first network roundtrip of the session is
  `start_transaction()`, which is awaited.
* Python 3.13.13 is required (on 3.14 the retro engine's pure-Python `Task`
  breaks `asyncio.timeout`/`current_task`); not a client finding.
