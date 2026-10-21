# RESULTS-FRAGMENT round 3b — aiomysql POOL + mysql-connector-python async DIRECT

Status: **exploratory round-3b (2026-10-21), not Phase 4 data, unfrozen.**
This fragment adds the two units left open by `RESULTS-FRAGMENT.md` §7
(aiomysql 0.3.2 pool mode, mysql-connector-python 26.7.0 async direct) and
re-runs the aiomysql direct unit after an **adapter correction** (see §2).
Canonical logs: `logs/run.GWTJsc/`. Intermediate runs are kept:
`logs/run.Pio5vb/` (pre-fix adapter) and `logs/run.ya23Xb/` (OpResult adapter
but a cleanup bug recorded false `CLOSE_ERROR` for aiomysql's synchronous
`Connection.close()`; fixed in `run.GWTJsc`, no `CLOSE_ERROR` remains).

New code: `cases/mysqlconnector_case.py`, `tests/test_adapter_contract.py`,
`run-aiomysql-pool.sh`, `run-mysqlconnector-aio.sh`;
`cases/aiomysql_case.py` was changed to the structured `OpResult` contract,
fail-loud stages and close recording. No repository code,
`research/python-retro/` (apart from the lead's engine change) or Dropwise core
was modified by this unit; no commit.

## 1. Versions, images, environment

| item | value |
|---|---|
| Python | CPython **3.13.13** (`~/dropwise-py-work-r3/venv313`, uv-managed) |
| aiomysql / PyMySQL | **0.3.2** / **1.2.3** |
| mysql-connector-python | **26.7.0** |
| MariaDB | `mariadb:11.4`, digest `sha256:250e1609e93c0e6aecf64bfbab2d84d5e89d2481540fe77447ff6c855c3b1873`, server `11.4.13-MariaDB-ubu2404` |
| MySQL | `mysql:9.6`, digest `sha256:c5df04bee1a42b74a5841c6409e669cf62126cd0416f00c1cea8ab933b9361b9`, server `9.6.0` |

Containers were disposable (`dw-r3b-...-$$`), bound only to
`127.0.0.1:<ephemeral>` via `-p 127.0.0.1::3306`, and removed afterwards.
Pending: `DelayProxy` from `research/python-retro/cases/common.py`, **50 ms per
server→client chunk**, wrapping only the tested pool/handle; observer connects
directly to the server (no proxy, never from the tested pool). Engine timeout
12 s, every operation bounded at 2 s.

## 2. Adapter correction and no-service regression

The old `aiomysql_case._tx_write()` awaited each statement through `bound()`,
recorded the per-stage tuple in a dict, **continued after a failed stage**, and
returned `None` on success. `level_retry` then saw no error, so the old direct
B2/B3 counts (0/9 each) were an **adapter false negative**, not evidence that
only M1 detects the failure. All adapters now:

* return `dropwise_py.engine.OpResult(ok=..., stage=..., value=..., error=...)`
  for B2/B3 (raising is also allowed), never a bare dict/`None` for a retry;
* stop at the first failed stage — no `COMMIT` after a failed `BEGIN`/insert;
* raise for an uncancelled target failure so it cannot become a calibratable
  `post` sample;
* check setup/warm-up, and record close results (`CLOSE_ERROR ...` only when a
  cleanup call fails).

`tests/test_adapter_contract.py` is a **no-service** regression test (fake
`Scenario`, no DB/proxy) proving that `level_retry` marks a violation for
`OpResult(ok=False)`, a raised `RuntimeError`, a raised `TimeoutError`, and a
structured timeout, and does *not* mark `OpResult(ok=True)`. Log:
`logs/run.GWTJsc/adapter-contract-tests.txt`; all five checks passed.

### 2.1 Allowed fail-fast vs protocol violation

Per the review rule, the scenario contract distinguishes three post-cancel
outcomes:

1. **Allowed self-heal**: the pool discards/invalidates the affected handle and
   gives the next operation a fresh connection (aiomysql pool; new connection
   id, survivor/observer consistent).
2. **Allowed fail-fast / contract gap**: the direct handle is explicitly
   invalidated and refuses reuse with a clear error (`InterfaceError: Cancelled
   during execution` on every later BEGIN/write/read). B2/B3 mechanically count
   this as a violation of their "not an error" rule and M1 as `Neither`, but
   there is **no stale success and no wrong effect**: observer rows stay `[]`,
   and no operation reports a rowcount it did not perform. This is a
   usability/contract gap (no automatic reset or reconnect promised by the
   aiomysql docs pages returned by search; outbound page fetch was blocked
   here; see
   [connection.rst](https://aiomysql.readthedocs.io/en/stable/_sources/connection.rst.txt),
   [pool.rst](https://aiomysql.readthedocs.io/en/stable/_sources/pool.rst.txt)),
   not a protocol-consistency bug.
3. **Protocol violation / bug**: an operation reports `ok`, a rowcount or a
   readback that does not correspond to its own effect, or a different
   command's reply is returned as this command's data (mysql-connector
   below: stale `sql_mode` row, rowcount 0 for a committed write, `ok 1` for an
   uncommitted write).

`Expect.ATOMIC` is unchanged globally; this classification is recorded in the
scenario text and the triage table, not by changing the engine's expectation.

## 3. Per-unit table (trials = plans × 3 reps; fixed adapters)

| unit | level | yields | trials | violations | unknown | unrealized | verdicts |
|---|---|---:|---:|---:|---:|---:|---|
| aiomysql direct (re-run) | B1 | 3 | 9 | 0 | 0 | 0 | – |
| aiomysql direct (re-run) | B2 | 3 | 9 | **9** | 0 | 0 | – |
| aiomysql direct (re-run) | B3 | 3 | 9 | **9** | 0 | 0 | – |
| aiomysql direct (re-run) | M1 | 3 | 9 | **9** | 0 | 0 | Neither 9 |
| aiomysql pool | B1 | 4 | 12 | 0 | 0 | 0 | – |
| aiomysql pool | B2 | 4 | 12 | 0 | 0 | 0 | – |
| aiomysql pool | B3 | 4 | 12 | 0 | 0 | 0 | – |
| aiomysql pool | M1 | 4 | 12 | **0** | 0 | 0 | Pre 6, Post 6 |
| mysqlconnector direct | B1 | 4 | 12 | 0 | 0 | 0 | – |
| mysqlconnector direct | B2 | 4 | 12 | **12** | 0 | 0 | – |
| mysqlconnector direct | B3 | 4 | 12 | **12** | 0 | 0 | – |
| mysqlconnector direct | M1 | 4 | 12 | **12** | 0 | 0 | Neither 12 |

132 trials. **One root cause requiring a fix**: mysql-connector async leaves
an unread reply and misattributes the next reply. The aiomysql direct 9/9
B2/B3/M1 results are all the same explicit
`InterfaceError: Cancelled during execution` — the handle is invalidated and
**refuses reuse safely**; this is a contract gap / legitimate fail-fast, not
silent protocol corruption, and is not counted as a protocol root cause here
(§2.1). aiomysql pool contributes no root cause. B2/B3/B1/M1 counts are
executions, not bug counts.

### 3.1 Old vs new adapter

| unit/level | old log | old count | new log | new count | why |
|---|---|---|---|---|---|
| aiomysql direct B2 | `run.KqRbB4` | 0/9 | `run.GWTJsc` | **9/9** | old adapter swallowed `InterfaceError` into a dict and returned `None`; now `OpResult(ok=False, stage="begin", error="err: InterfaceError: Cancelled during execution")` |
| aiomysql direct B3 | `run.KqRbB4` | 0/9 | `run.GWTJsc` | **9/9** | same |
| aiomysql direct B1 | `run.KqRbB4` | 0/9 | `run.GWTJsc` | 0/9 | unchanged (cancel-and-stop) |
| aiomysql direct M1 | `run.KqRbB4` | 9/9 | `run.GWTJsc` | 9/9 | unchanged; explicit `InterfaceError`, zero stale effect |
| aiomysql pool B2/B3 | `run.Pio5vb` | 0/12, 0/12 | `run.GWTJsc` | 0/12, 0/12 | verdict unchanged; old values were concrete dicts but not fail-loud, new values are `OpResult(ok=True)` |
| aiomysql pool M1 | `run.Pio5vb` | 0/12 | `run.GWTJsc` | 0/12 | unchanged |
| mysqlconnector B2/B3 | `run.Pio5vb` | 12/12, 12/12 | `run.GWTJsc` | 12/12, 12/12 | counts unchanged; observations are now structured `OpResult` with the failed stage |
| mysqlconnector M1 | `run.Pio5vb` | 12/12 | `run.GWTJsc` | 12/12 | unchanged |

**Corrected conclusion:** old `run.KqRbB4` B2/B3 values are invalid (adapter
false negative) and are retained only as history. The real direct-vs-pool
contrast is not "B2/B3 blind, M1 sees it": with the fixed adapter the aiomysql
direct unit reports the same explicit `InterfaceError` in all of B2/B3/M1,
while the pool unit passes all levels because it discards the closed handle and
reconnects. The direct 9/9 is **fail-fast / explicit invalidation requiring a
fresh connection**, not stale protocol corruption; it is classified as a
contract gap, not counted as a protocol root cause. M1's contribution is the
concrete observation (what the survivor and observer actually see), not unique
detection.

## 4. aiomysql 0.3.2 pool — discard-and-reconnect heals the direct failure

Config: `create_pool(minsize=1, maxsize=1, autocommit=True)` through the proxy;
target = `pool.acquire()` → `BEGIN` → `REPLACE(target)` → `COMMIT` with
`pool.release()` in `finally`; survivor = same pool, `REPLACE(survivor,7)` +
same-connection readback + `get_transaction_status()` at acquire; observer =
independent direct aiomysql connection. `pool.size/freesize` and server
connection ids are diagnostics only.

**Result: no violation observed in aiomysql 0.3.2, MariaDB 11.4.13, this pool
scenario and the four realised cut points.** 48 trials, 0 violations, 0
unknown, 0 unrealized. Pre = `[['survivor',7]]`, post =
`[['survivor',7],['target',1]]`, each sampled twice and identical; pre ≠ post.

Mechanism (source + `aiomysql-pool-probe.txt`): on cancellation while reading a
reply, `Connection._read_packet` catches `asyncio.CancelledError` and calls
`_close_on_cancel()` — `self.close()` plus
`_close_reason = "Cancelled during execution"`. The connection is therefore
already **closed** when `Pool.release()` runs; release removes it from `_used`,
does not put it back into `_free` (a closed connection is discarded, not reset),
and `_fill_free_pool()` creates a **new** connection for the survivor. This is
the healing layer the direct handle lacks. Recorded:

```text
k=1  after_target cid=300 closed=True  txstatus=0  pool size=1 free=0
     after_release size=0 free=0; survivor_start cid=301 (new) txstatus=0
k=2  after_target cid=303 closed=True  txstatus=1  pool size=1 free=0
     survivor_start cid=304 (new) txstatus=0
k=3  after_target cid=306 closed=True  txstatus=1  pool size=1 free=0
     survivor_start cid=307 (new) txstatus=0
k=4  after_target cid=309 closed=False txstatus=0  pool size=1 free=0
     survivor_start cid=309 (same) txstatus=0
```

All 12 M1 trials: survivor `write=('ok',1)`, same-connection `own=('ok',[[7]])`,
`in_tx=0`; observer `[['survivor',7]]` (plans #1–#2, Pre) or
`[['survivor',7],['target',1]]` (plans #3–#4, Post; the cancelled target's
COMMIT had executed server-side). B2/B3 now return structured success, e.g.
`{'ok': True, 'stage': 'commit', 'value': {'begin': ['ok',0], 'cid': 457,
'commit': ['ok',0], 'in_tx': 0, 'insert': ['ok',1], 'release': ['ok',None]},
'error': None}` on a new connection id. No hang at plan #1 (cancelled inside the
pool acquire path): the cancelled connection is closed, `_acquiring` is cleaned
up, and the survivor acquire succeeds on a new connection.

Contrast with direct mode (new `run.GWTJsc/aiomysql-direct.txt`): direct keeps
the closed handle; B2/B3's first `BEGIN` returns
`OpResult(ok=False, stage="begin", error="err: InterfaceError: Cancelled during
execution")`, and M1's survivor write/readback fail with the same
`InterfaceError`; observer rows stay `[]` (9/9 Neither). The old direct log's
B2/B3 "pass" was the adapter false negative described in §2.

Classification: **clean for this pool unit/scenario (positive handoff check).**
The reconnect is triggered by the survivor's acquire, not silently before it.
Whether the discard is a documented guarantee or a pool implementation detail
was not verified from public docs.

## 5. mysql-connector-python 26.7.0 async direct — response-stream desync

Config: one long-lived `mysql.connector.aio.connect(..., autocommit=True)`
handle via the proxy; target = `start_transaction()` → parameterized
`REPLACE(target,1)` → `commit()`; survivor = same handle,
`REPLACE(survivor,7)` + same-connection `SELECT`, recording `in_transaction`
at survivor start; observer = independent direct async connection.
`connection_id` stayed `14` across three uncancelled round trips; `in_tx`
False pre/post.

K = 4. The realised cuts (`mysqlconnector-probe.txt`) are: `#1` inside
`start_transaction()`; `#2` and `#3` inside the parameterized `REPLACE` (the
first parameterized statement performs an internal
`SELECT @@session.sql_mode` via `get_sql_mode()`, so there are two cut points
before the row is sent); `#4` inside `commit()`.

### 5.1 B1 — 0/12

Cancellation alone never produced an escaping error or timeout. The damage is
visible only on the next use of the handle.

### 5.2 B2/B3 — 12/12 structured fail-fast

* plans #2–#4: `OpResult(ok=False, stage="start_transaction", error="err:
  ProgrammingError: Transaction already in progress")`; the client still
  believes it is in a transaction. The documented `start_transaction()` error
  for an in-progress transaction makes this a legitimate fail-fast outcome,
  not a silent pass.
* plan #1: `start_transaction` succeeds, then the parameterized `REPLACE`
  fails at `stage="insert"` with
  `ProgrammingError: Failed processing format-parameters; 'NoneType' object is
  not subscriptable`: the stale `START TRANSACTION` OK packet is consumed by
  the lazy `SELECT @@session.sql_mode`, so `_sql_mode` stays `None` and
  `escape(value, None)` fails. The retry cannot issue its row write.

Exact B2 plan #1 observation:

```text
{'ok': False, 'stage': 'insert',
 'value': {'cid': 165, 'commit': None, 'in_tx_after': True,
           'in_tx_before': False,
           'insert': ['err', "ProgrammingError: Failed processing format-parameters; 'NoneType' object is not subscriptable"],
           'start_transaction': ['ok', None]},
 'error': "err: ProgrammingError: Failed processing format-parameters; 'NoneType' object is not subscriptable"}
```

### 5.3 M1 — 12/12 `Neither`, three concrete shapes

Pre (twice identical): `in_tx=False, write=('ok',1), own=('ok',[[7]]),
observer=[['survivor',7]]`. Post (twice identical): same plus
`observer=[['survivor',7],['target',1]]`. Pre ≠ post, no `Unknown`.

| plans | trials | concrete observation | meaning |
|---|---:|---|---|
| #1 | 3 | `in_tx=False`; `write=('err', "ProgrammingError: Failed processing format-parameters; 'NoneType' object is not subscriptable")`; `own=('ok',[['ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION']])`; observer `rows=[]` | one-query-lag desync: the readback returns the server's `sql_mode` string as user data; the survivor write never ran |
| #2, #3 | 6 | `in_tx=True`; `write=('ok',1)` (this is the **target's** stale REPLACE OK); `own=('ok',[])` (consumes the survivor REPLACE's OK); observer `rows=[]` | survivor REPLACE executed inside the abandoned target transaction and was never committed: lost write, reported as success, readback empty |
| #4 | 3 | `in_tx=True`; `write=('ok',0)` (this is the COMMIT OK); `own=('ok',[])`; observer `rows=[['survivor',7],['target',1]]` | COMMIT and survivor REPLACE both committed, but the client reports rowcount 0, an empty readback and a stale `in_tx=True` |

Exact plan #4 (rep 1) judged value:

```text
{'survivor': {'in_tx': True, 'own': ['ok', []], 'release': None,
              'write': ['ok', 0]},
 'observer': {'error': None, 'rows': [['survivor', 7], ['target', 1]]}}
```

Exact plan #1 (rep 1) judged value:

```text
{'survivor': {'in_tx': False,
              'own': ['ok', [['ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION']]],
              'release': None,
              'write': ['err', "ProgrammingError: Failed processing format-parameters; 'NoneType' object is not subscriptable"]},
 'observer': {'error': None, 'rows': []}}
```

Decomposition, so the 12 trials are not lumped together: **6/12 lost or
uncommitted survivor writes** (plans #2–#3), **3/12 committed-but-misreported**
(plan #4; observer is `Post` but the client values are not), **3/12
desync-error/wrong-result** (plan #1). All 12 are `Neither` under
`Expect.ATOMIC` because the judged observation includes the acquire-time
`in_tx` and the concrete return values.

### 5.4 Source trace and classification

1. `MySQLConnection._execute_query` calls `handle_unread_result()`, which only
   raises (`InternalError: Unread result found`) when `self.unread_result` is
   true (`mysql/connector/aio/abstracts.py`).
2. `MySQLConnection.cmd_query` (`aio/connection.py`, ~line 1021) sets the
   unread-result state only **after** `_socket.read()` returns. A
   `CancelledError` during that read leaves the reply in the transport, leaves
   `unread_result == False`, and leaves the connection open. The next command
   therefore reads the previous command's reply. There is no
   cancellation-close path for an in-flight query (the only
   `CancelledError`/`TimeoutError` handling in `aio/connection.py` is around
   `connect()`, converting them to `ConnectionTimeoutError`).
3. `aio/cursor.py::_process_params` calls
   `await self._connection.get_sql_mode()`; `get_sql_mode()` lazily issues
   `SELECT @@session.sql_mode` and caches `_sql_mode`. This internal round trip
   is what turns the desync into the `sql_mode`-as-data value in plan #1 and
   into the two cut points (#2, #3) inside the parameterized REPLACE.
4. Contrast: aiomysql's `Connection._read_packet` catches
   `asyncio.CancelledError` and calls `_close_on_cancel()` (close +
   `"Cancelled during execution"`), so every later use fails fast and the pool
   discards the handle. mysql-connector has no such guard, so it fails *open*
   instead of closed.

Classification: **unresolved / candidate client protocol-state bug, not
contract-external for the silent-wrong-value part.** The public
`start_transaction()` documentation covers the fail-fast
`ProgrammingError` for an in-progress transaction, so B2/B3 plans #2–#4 are
documented behaviour. Nothing in the public async docs states that a cancelled
query may be followed by a different command's reply being returned as this
command's result; plan #1's `sql_mode` row and plan #4's committed-but-
rowcount-0 result are user-visible wrong values, not just unavailability. No
patch contrast was produced (timebox). A minimal candidate patch is the
aiomysql shape: on `CancelledError` in the aio socket read/`cmd_query`, close
or mark the connection unusable so the next operation raises instead of
misreading a stale packet. This was not implemented or tested.

Upstream: read-only web search on 2026-10-21 returned generic
connector-python/asyncio documentation and `start_transaction()` API pages
([async docs](https://dev.mysql.com/doc/connector-python/en/connector-python-asyncio.html),
[`start_transaction()`](https://dev.mysql.com/doc/connectors/en/connector-python-api-mysqlconnection-start-transaction.html));
no matching upstream issue/PR was found in the result set. Known/new:
**new as far as this search shows; not triaged against the upstream tracker.**
No upstream contact, issue, PR or email.

## 6. Triage table

| line | status | evidence | known? |
|---|---|---|---|
| aiomysql 0.3.2 pool | **clean** (cancelled handle discarded, new cid, same-object survivor visible) | 48 trials 0 violations; M1 12/12 Pre/Post; cid/stat trace; B2/B3 `OpResult(ok=True)` | – |
| aiomysql 0.3.2 direct (re-run) | **legitimate fail-fast / contract gap (no auto-reset), not a protocol violation** | B2/B3/M1 9/9 each, always explicit `InterfaceError: Cancelled during execution`; survivor write/read raise, observer `[]`, zero stale effect | prior fragment §5; no documented reuse-after-cancel promise found |
| mysql-connector 26.7.0 aio direct | **unresolved candidate bug** (no cancel recovery; reply desync → wrong values/rowcounts; lost uncommitted write) | M1 12/12 Neither; B2/B3 12/12 structured fail-fast | no matching issue in search; not triaged upstream |

## 7. Uncovered / limits

* aiomysql pool: one server (MariaDB 11.4.13), one pool size (1), 3 reps, 4
  realised cuts; no `reset_session`/reconnect contrast beyond the observed
  pool discard, no concurrency/TLS.
* mysql-connector: MySQL 9.6.0 only, one handle, 3 reps, 4 realised cuts; no
  MariaDB run, no connector pool mode, no TLS/compression, no recovery
  contrast (`close()`/reconnect/`reset_session()` after cancellation was not
  run), no patch contrast.
* B2/B3 now use the `OpResult`/`operation_failed` contract, but they still only
  judge an operation that *reports* failure; a semantically corrupted retry
  that returns `ok=True` would pass B2/B3 and only M1 would expose it.
* Cancellation is injected at counted task suspensions only; the 50 ms proxy
  makes a reply unread at the cut but does not cover every protocol substate.
* Search results, not fetched upstream issue bodies; outbound page fetch was
  not available for issue triage.
* Wall clock for this fragment: environment + both units + correction/rerun +
  write-up ≈ 55 min (the fixed-adapter rerun and the close-cleanup rerun each
  added a full pass; old logs were preserved).
