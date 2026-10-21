# Round-3 Python results fragment: pymongo 4.19.0 + neo4j 6.4.1 (pool units)

Status: **unfrozen exploration, 2026-10-21.** Not Phase 4 data, no
preregistration, no commits, no upstream contact. `research/python-retro/` was
read only (engine and delay proxy imported from there, nothing modified).

Batch dir: `prospective/phase4-exploration/python/mongo-neo4j/`.

**Valid (counted) run dirs**

| service | run dir | content |
|---|---|---|
| MongoDB 7.0.21 | `logs/run.Qhri8J/` | `mongo-{txn,plain}.txt` (B1/B2/B3/M1 ×3), `*-diag.txt`, `*-cancelled-probe.txt`, `environment.txt` |
| Neo4j 5.26.0 | `logs/run.xu9AhN/` | `neo4j-{txn,plain}.txt`, `*-diag.txt`, `*-cancelled-probe.txt`, `environment.txt` |

**Superseded / not counted.** `logs/run.ghhvJH/` (first MongoDB pass) and
`logs/run.VIRLyE/` (first Neo4j pass) started at 22:45 before the structured
`OpResult` engine landed (engine mtime 22:46:31) and used the old dict/`"ERR "`
adapters. They are kept as raw history, not as evidence; every number below is
from the two valid runs.

## 1. Scope executed

| package | version | unit | levels × reps | executed trials |
|---|---|---|---|---:|
| pymongo | 4.19.0 | pool `txn` (`AsyncMongoClient(maxPoolSize=1)`) | B1, B2, B3, M1 × 3 | 48 |
| pymongo | 4.19.0 | pool `plain` | B1, B2, B3, M1 × 3 | 24 |
| neo4j | 6.4.1 | pool `txn` (`AsyncGraphDatabase` `max_connection_pool_size=1`) | B1, B2, B3, M1 × 3 | 48 |
| neo4j | 6.4.1 | pool `plain` | B1, B2, B3, M1 × 3 | 24 |

144 executed trials total (108 B1/B2/B3 + 36 M1). Each unit also ran one
uncancelled `count_yields` target per level and two pre + two post calibration
samples for M1 (16 calibration scenario runs); these are not trials.

## 2. Versions, environment, images

- Python **3.13.13** (`~/dropwise-py-work-r3/venv-py313/bin/python`).
  pymongo **4.19.0** (dnspython 2.9.0), neo4j **6.4.1** (pytz 2026.5).
- MongoDB image `mongo:7.0.21`,
  `sha256:3d715950d83061ff2fbc910d12d3703212538cacf6b3003e3736fa5c7f51a2e1`;
  single-node replica set `rs0`, `--network host --replSet rs0
  --bind_ip 127.0.0.1 --port 57017`, `rs.initiate` member
  `127.0.0.1:57017`, waited for `isWritablePrimary=true`, server 7.0.21.
  Transactions need the replica set; the driver used
  `directConnection=true` through the proxy port.
- Neo4j image `neo4j:5.26.0-community`,
  `sha256:5a015e53de1895e7eee1574ae0325cf8c4b89587222778108c594bdd45a474b5`;
  kernel 5.26.0 community, Bolt on `127.0.0.1:57687`, HTTP `127.0.0.1:57474`,
  `NEO4J_AUTH=neo4j/testpassword` (basic auth, **not** `none`). `bolt://`
  (direct pool, no routing).
- Pending: read-only `DelayProxy`, **50 ms per forwarded chunk in both
  directions**, wrapping only the tested client/driver; observer direct.
- Bound 2.0 s per operation, engine timeout 12 s, `Expect.ATOMIC`, reps 3.
- Both containers were `--rm` and removed by the runner `EXIT` traps; the
  cancelled-trial probe used two further disposable containers, also removed.
  Final `docker ps` had no `dw-r3-py-*` container.

## 3. Prior exposure

MongoDB (phase 3 `WriteConflict`) and Neo4j/BOLT (round 2, bolt-client) server
families were already touched. These are **current-driver extensions of touched
server families**, not blind protocol families. Neither driver's asyncio
cancellation/pool path had been exercised before.

## 4. Per-unit ladder table

All values are from the valid runs. `pre != post` in all four M1 units; every
trial realized its requested cut (`unrealized=0`), no `Unknown`, no baseline
error.

| unit | level | yields | trials | violations | unknown | unrealized | seconds |
|---|---|---:|---:|---:|---:|---:|---:|
| pymongo txn | B1 | 4 | 12 | 0 | 0 | 0 | 8.72 |
| | B2 | 4 | 12 | 0 | 0 | 0 | 11.23 |
| | B3 | 4 | 12 | 0 | 0 | 0 | 10.01 |
| | M1 | 4 | 12 | 0 | 0 | 0 | 14.13 |
| pymongo plain | B1 | 2 | 6 | 0 | 0 | 0 | 3.96 |
| | B2 | 2 | 6 | 0 | 0 | 0 | 4.58 |
| | B3 | 2 | 6 | 0 | 0 | 0 | 4.57 |
| | M1 | 2 | 6 | 0 | 0 | 0 | 7.90 |
| neo4j txn | B1 | 4 | 12 | 0 | 0 | 0 | 9.12 |
| | B2 | 4 | 12 | 0 | 0 | 0 | 16.37 |
| | B3 | 4 | 12 | 0 | 0 | 0 | 13.83 |
| | M1 | 4 | 12 | 0 | 0 | 0 | 20.36 |
| neo4j plain | B1 | 2 | 6 | 0 | 0 | 0 | 4.07 |
| | B2 | 2 | 6 | 0 | 0 | 0 | 6.53 |
| | B3 | 2 | 6 | 0 | 0 | 0 | 6.61 |
| | M1 | 2 | 6 | 0 | 0 | 0 | 11.37 |

**No violation observed** in these versions, scenarios and realised cancellation
points. Violations by plan are `[]` for every level/unit. This is a coverage
statement, not "clean" or "proven safe".

## 5. Concrete M1 values (all reps identical per plan)

Survivor value is the structured `OpResult` (`ok`, `stage`, `value`); observer
is an independent direct client.

| unit | plan | verdict | survivor concrete value | observer concrete value |
|---|---|---|---|---|
| pymongo txn | pending#1, #2 | Pre | write ok `matched=0, upserted=survivor`; own `{_id: survivor, v: 7}` | docs `[[survivor, 7]]` |
| pymongo txn | pending#3, #4 | Post | same survivor write/readback | docs `[[survivor, 7], [target, 1]]` |
| pymongo plain | pending#1, #2 | Post | write ok `matched=0, upserted=survivor`; own `{_id: survivor, v: 7}` | docs `[[survivor, 7], [target, 1]]` |
| neo4j txn | pending#1, #2, #3 | Pre | write ok `{k: survivor}`; own `{v: 7}` | nodes `[[survivor, 7]]` |
| neo4j txn | pending#4 | Post | same survivor write/readback | nodes `[[survivor, 7], [target, 1]]` |
| neo4j plain | pending#1 | Pre | write ok `{k: survivor}`; own `{v: 7}` | nodes `[[survivor, 7]]` |
| neo4j plain | pending#2 | Post | same | nodes `[[survivor, 7], [target, 1]]` |

`pre` (2/2 samples) = survivor only, `post` (2/2) = survivor + target; the two
samples on each side agreed exactly. For the 4-yield units the cut at the
commit await is `pending#4` (`#3` for Neo4j txn is the `single()` await); a
cancelled commit can still commit, which is the legal `Post` observed.

### Transaction committed or aborted? state still attached? (cancelled probe)

The `*-cancelled-probe.txt` files run the target once per cut and print the
`diag` recorded in the target's `finally`:

* **pymongo txn, cuts at commit (`k=3,4`)**: `target_final` state
  `COMMITTED`, `in_tx=False`, `ended=False`; `abort_if_open="not_in_tx"`;
  `end_session=("ok", None)`; `after_target` state `COMMITTED`, `ended=True`.
  The transaction really committed (observer `Post`), the session ended, and
  the pool had `conns=0, ids=[]` at survivor start — **the sole socket was
  discarded, not returned**; the survivor opened a new connection and its write
  and readback succeeded. This is legal self-heal, not a bug: data is `Post`.
* **pymongo plain, cuts `k=1,2`**: no session object; `survivor_start` pool
  `conns=0, ids=[]`; the survivor reconnects and succeeds; observer `Post`.
* **neo4j txn, cuts `k=3,4`**: `target_final` `tx.closed()=True`, rollback
  `"already_closed"`, `session_close=("ok", None)`, pool `in_use=0, idle=1`
  with the **same connection object id** before and after
  (`after_target.ids == setup_ids`). Observer `Pre` at `k=3`, `Post` at `k=4`.
  The connection was returned to the pool and reused.
* **neo4j plain, cuts `k=1,2`**: `tx=None`, `session_close` ok, pool
  `in_use=0, idle=1`, same connection id; observer `Pre` at `k=1`, `Post` at
  `k=2`.

So on cancellation the Mongo driver discards the interrupted socket (and
reconnects for the survivor), while the Neo4j driver returns the same
connection after rolling back or committing. Neither shape hides the
transaction outcome: the observer distinguishes committed from aborted, and
the survivor is a real same-client/same-driver operation.

## 6. B2/B3 retry values and the strict adapter

`same()`/`different()` now return
`dropwise_py.engine.OpResult(ok=..., stage=..., value=..., error=...)`, and
`level_retry` uses `operation_failed()` (structured `ok`, not the historical
`"ERR "` string prefix). All B2/B3 trials: `ok=True`; no nested `err`/`timeout`
in any retry value.

Examples (identical across reps, sanitised):

* pymongo txn B2 `pending#3` → `OpResult(ok=True, stage='txn', value={...,
  'commit': 'ok', 'state_after_commit': {'tx': 'COMMITTED', ...}})`; B3 →
  `OpResult(ok=True, stage='plain', value={'write': ..., 'upserted': 'b3'})`.
* pymongo plain B2 → `OpResult(ok=True, stage='plain', ...upserted 'target')`;
  B3 → `update ... upserted 'b3'`.
* neo4j txn B2 → `OpResult(ok=True, stage='txn', value={'commit': 'ok',
  'commit_bookmark': 'FB:…', 'tx_after_commit': {'closed': True}, ...})`; B3 →
  `stage='plain'` MERGE b3.
* neo4j plain B2 → `stage='plain'`, record `{k: target}`, consume `True`; B3 →
  `CREATE` b3, record `{k: b3}`.

## 7. Reuse / reconnect evidence

* MongoDB: the tested client's sole pooled socket identity was unchanged over
  three `ping`s in both diag runs (`ids=[<one id>]` three times,
  `conns=1, active=0, max=1`), i.e. genuine reuse, not a new connection per
  operation. After a cancelled target the cancelled-probe shows `conns=0,
  ids=[]` and the survivor's write creates a new socket — the driver discarded
  the interrupted one, and no reconnect happened *before* the survivor.
* Neo4j: one Bolt connection object id across three pings and after the
  cancelled target; `in_use=0, idle=1` at survivor start, same id in
  `after_target.ids` — returned to the pool and reused by the survivor, not
  discarded.

## 8. Structural audit (adapter error-swallowing)

Requested after the aiomysql/asyncpg finding.

* Engine (`research/python-retro/dropwise_py/engine.py`, read-only): `OpResult`
  and `operation_failed()`; `normalize()` renders `OpResult` structurally;
  `level_retry` flags a trial when `operation_failed(value)` is true.
* Both adapters were rewritten: `target`, `same`, `different`, `survivor` and
  `observe` return `OpResult` (or raise). `op_from()` marks the first
  `err`/`timeout` stage as `ok=False` with `stage=<op>.<stage>`.
* `target()` **raises** if the uncancelled target completed with a failed
  stage, so a broken baseline cannot make `post` look like `pre` (the silent
  pass shape). `same`/`different` return `ok=False` instead of raising, so
  `operation_failed()` sees the failure; a retry that exceeds its 2 s stage
  bound or the engine timeout is a violation.
* Setup/warm-up/connect failures raise `RuntimeError` (observer, tested
  client/driver, warm-up). Acquire is per-operation; a failed acquire/write
  becomes `OpResult(ok=False)`. Cleanup (`abort_transaction`, `end_session`,
  `tx.rollback`, `session.close`) is bounded, recorded in `diag` and in
  `OpResult.value["cleanup"]`, and never swallowed silently.
* No-service test `cases/test_structural.py` (fake scenario, no network):
  7/7 pass — `OpResult(ok=False)` and `{"ok": False}` are failures, a failed
  `same()` is a B2 violation, a timed-out retry is a B3 violation, a raising
  target is a B1 violation, and a clean retry has none.
  Command: `PYTHONPATH=".:<repo>/research/python-retro" python -m cases.test_structural`.
* Old runs (`run.ghhvJH`, `run.VIRLyE`) used the pre-audit dict adapters and
  (partly) the pre-`OpResult` engine; they are **invalid for counting** and are
  kept only as raw history. All numbers in §4–§7 are from the valid rerun.
* Legitimate abort/rollback/connection invalidation was not reclassified as a
  bug: Mongo commit-at-cancel → observer `Post` + session ended; Neo4j
  rollback/commit → `tx.closed()=True`, same connection returned. `Expect.ATOMIC`
  is unchanged.

## 9. Triage / upstream

Zero violations across 144 executed trials, so there is no candidate to
triage, no source trace, no patch contrast and no upstream issue search. This
is a negative/coverage result: "no violation observed in pymongo 4.19.0 /
neo4j 6.4.1, this scenario, these cancellation points". It does not establish
safety for pool sizes > 1, concurrency, TLS/routing, other server versions, or
cancellation at driver-internal points that are not task suspensions.

## 10. Coverage gaps and limits

* Cancellation points are the target coroutine's task suspensions only
  (pymongo 4.19/neo4j 6.4 expose several: connect/begin/write/single/commit).
  Driver internals not awaited by the target (pymongo's background topology
  monitor, socket-reader callbacks) are not individually cancellable; one
  `Task.cancel()` per run, reps 3, no concurrent operations.
* B2/B3 are one retry, not a recovery policy; semantic corruption of a retried
  transaction is judged only through the structured per-stage `ok` and the
  concrete values, not by a full transaction-state oracle.
* `AsyncSession.execute_query` does not exist in neo4j 6.4.1 (introspection:
  only `execute_read`/`execute_write`/`run`), so the explicit
  `begin_transaction()`+`run` path is the one covered; `execute_write` retry
  callbacks were not exercised.
* MongoDB is a single-node replica set; no sharded topology, no read concern /
  write concern variations, no failpoints. Neo4j is a single community server,
  `bolt://` direct pool, no routing/cluster, no TLS.
* Both units are pool size 1 and loopback-only; no pool-size sweep, no
  cross-process concurrency, no observer latency injection.
* Python 3.13.13 is required by the retro engine (3.14 breaks the pure-Python
  `Task`/`current_task` check); not a client finding.
* Old first-pass logs exist with the old adapter and must not be cited.

## 11. Cost and cleanup

Wall clock about 25 minutes for the first ladder pass plus ~15 minutes for the
structural audit, adapter rewrite, structural test and full rerun, plus two
cancelled-trial probe containers (~3 minutes). Logs are under the two valid run
dirs; the four superseded/valid dirs are the only new `logs/run.*` entries.
All MongoDB/Neo4j containers, proxies and processes started here were removed;
no commits, pushes, issues, PRs or emails were made.

## 12. Paths

* scenario: `prospective/phase4-exploration/python/mongo-neo4j/SCENARIO.md`
* runners: `run-mongo.sh`, `run-neo4j.sh`
* cases: `cases/common.py`, `cases/mongo_case.py`, `cases/neo4j_case.py`,
  `cases/test_structural.py`
* valid logs: `logs/run.Qhri8J/`, `logs/run.xu9AhN/`
* superseded logs: `logs/run.ghhvJH/`, `logs/run.VIRLyE/`
