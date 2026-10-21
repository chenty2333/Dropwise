# RESULTS-FRAGMENT — PostgreSQL/MySQL Python clients (round 3, unfrozen exploration)

Status: **exploratory, 2026-10-21, not Phase 4 data.** 4 packages / 7 reuse units
were in scope; this fragment covers **psycopg pool + direct, asyncpg pool +
direct, aiomysql direct** (5 units). aiomysql pool and
mysql-connector-python were **not run** (timebox); no claim is made about them.

> **Correction (round 3, later same day):** the psycopg/asyncpg **B2/B3 0
> values below are invalid as evidence** — `_tx_write()` did not propagate
> failed stages (`dict` without `ok` / `None`), so `level_retry` could not see a
> real error. Corrected reruns (`logs/run.1HLY38/`) reproduce 0 stage failures
> but only those logs support it; M1 counts are unchanged. See
> [RESULTS-FRAGMENT-fixed.md](RESULTS-FRAGMENT-fixed.md). aiomysql is another
> agent's scope.

## 1. Versions, images, environment

| item | value |
|---|---|
| Python | CPython **3.13.13** (uv managed), venv `~/dropwise-py-work-r3/venv313` |
| psycopg / psycopg-binary | **3.3.6** |
| psycopg-pool | **3.3.3** |
| asyncpg | **0.32.0** |
| aiomysql | **0.3.2** (pymysql 1.2.3) |
| mysql-connector-python | **26.7.0** (installed, not exercised) |
| PostgreSQL | `postgres:18-alpine`, digest `sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa` (server 18.4) |
| MariaDB | `mariadb:11.4`, digest `sha256:250e1609e93c0e6aecf64bfbab2d84d5e89d2481540fe77447ff6c855c3b1873`; `root` + password auth worked, no plugin issue |
| MySQL | `mysql:9.6`, digest `sha256:c5df04bee1a42b74a5841c6409e669cf62126cd0416f00c1cea8ab933b9361b9` (container started, unused) |

All servers bound to `127.0.0.1` only, disposable containers
(`dw-r3-py-...-$$`), removed on exit. Logs in `logs/run.KqRbB4/`.

**Python 3.13 is required by the retro engine, not by the clients.** On 3.14.8
the pure-Python `_CutTask` no longer satisfies `asyncio.timeout`'s
`current_task()` check, so a nested `asyncio.wait_for` inside the target raises
`RuntimeError: Timeout should be used inside a task`, yields stay 0 and every
plan is unrealized. That is an engine/runtime limit recorded here, not a target
finding. All reported numbers are 3.13.13.

Setup per run: fresh pool/handle through a `DelayProxy` (50 ms per
server→client chunk), fresh observer connection direct to the server (no proxy,
not from the pool), fresh unique table created by the observer, one warm-up
round trip. Target = `BEGIN` / write target row / `COMMIT` (three awaits in one
target coroutine). Survivor = same pool/handle, write survivor row in the same
table + same-connection readback, plus the connection state it was handed.
Observer = `SELECT k,v ORDER BY k` on the independent connection. Every op is
bounded with `asyncio.wait_for(..., 2 s)`; the engine's 12 s timeout is a
backstop. `Expect.ATOMIC`, 3 reps per plan.

## 2. Per-package / per-mode results (trials = plans × reps)

| unit | level | yields | trials | violations | unknown | unrealized | verdicts |
|---|---:|---:|---:|---:|---:|---:|---|
| psycopg pool | B1 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg pool | B2 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg pool | B3 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg pool | M1 | 6 | 18 | **0** | 0 | 0 | Pre 12, Post 6 (state field: IDLE on all 18) |
| psycopg direct | B1 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg direct | B2 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg direct | B3 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg direct | M1 | 6 | 18 | **12** | 0 | 0 | Neither 12, Post 6 (plans #1–#4 × 3) |
| asyncpg direct | B1 | 3 | 9 | 0 | 0 | 0 | – |
| asyncpg direct | B2 | 3 | 9 | 0 | 0 | 0 | – |
| asyncpg direct | B3 | 3 | 9 | 0 | 0 | 0 | – |
| asyncpg direct | M1 | 3 | 9 | **9** | 0 | 0 | Neither 9 (plans #1–#3 × 3) |
| asyncpg pool | B1 | 4 | 12 | 0 | 0 | 0 | – |
| asyncpg pool | B2 | 4 | 12 | 0 | 0 | 0 | – |
| asyncpg pool | B3 | 4 | 12 | 0 | 0 | 0 | – |
| asyncpg pool | M1 | 4 | 12 | **0** | 0 | 0 | Pre 6, Post 6 |
| aiomysql direct | B1 | 3 | 9 | 0 | 0 | 0 | – |
| aiomysql direct | B2 | 3 | 9 | 0 | 0 | 0 | – |
| aiomysql direct | B3 | 3 | 9 | 0 | 0 | 0 | – |
| aiomysql direct | M1 | 3 | 9 | **9** | 0 | 0 | Neither 9 (plans #1–#3 × 3) |

Executions reported here: 300 trials (72 per psycopg unit × 2, plus 36 psycopg
M1 state reruns, 84 asyncpg, 36 aiomysql; calibrations and count-yields runs not
counted as trials). **Independent root causes: 3** (one per
psycopg-direct/asyncpg-direct/aiomysql-direct family); 12/18, 9/9 and 9/9 are
executions, not bug counts.

Pre/post calibration: in every unit the two `pre` samples agree (survivor row
only: `[['survivor', 7]]`) and the two `post` samples agree (survivor + target:
`[['survivor', 7], ['target', 1]]`), so `pre != post` and the observation is not
degenerate. Unknown = 0 everywhere; unrealized = 0.

## 3. psycopg: pool repairs, direct leaks

### 3.1 Pool (positive handoff check)

All 18 M1 trials: survivor `write=('ok','INSERT 0 1')`, same-connection
`own=7`, observer sees `[['survivor',7]]` (plans #1–#4) or
`[['survivor',7],['target',1]]` (plans #5–#6, where the target's COMMIT had
already executed server-side — a cancelled target that committed is legal).
The survivor was handed `info.transaction_status = IDLE` in every trial.

Mechanism, observed: after a cancelled target the connection is
`INTRANS` before return, and **the pool itself rolls it back and logs it**:

```
rolling back returned connection: <psycopg.AsyncConnection [INTRANS] (...)>
```

then the survivor acquires the same backend pid in `IDLE`. This is the
"handoff check reads the signal" case: the state signal exists in
`ReadyForQuery`, the pool reads it on return. The pool unit is clean for this
version, scenario and realised cut points. (Whether that rollback is a
documented guarantee or an implementation detail was not verified: outbound
page fetch was blocked in this environment; the log line and the
INTRANS→IDLE transition are the recorded evidence.)

### 3.2 Direct handle (state leak, not judged by B2/B3)

Target cancelled at plans **#1–#4** leaves the same connection `INTRANS`
(`after_target.status=INTRANS`, same backend pid). The survivor acquires that
connection with `in_tx=INTRANS`, issues a normal single autocommit
`INSERT (survivor, 7)`: it returns **`INSERT 0 1`**, the same-connection
readback returns **7**, and the independent observer returns **`rows=[]`**.
The write was executed inside the abandoned transaction and is never committed;
it disappears when the connection closes. That is `Neither` ⇒ **12/18 M1
violations**, all in plans #1–#4 × 3 reps. Plans #5–#6 are `Post` (target COMMIT
had executed; target row visible).

Exact observation (plan #1, rep 1, state-augmented run):

```text
survivor: in_tx=INTRANS, write=('ok','INSERT 0 1'), own=('ok',7), release=('ok',None)
observer: rows=[]                                                     # pre=[['survivor',7]], post=[['survivor',7],['target',1]]
```

B2/B3 pass (0 violations) but their concrete values expose the same state:
`status_after_acquire=INTRANS`, `begin/insert/commit` all `ok`,
`status_before_release=IDLE`. So the retry's `COMMIT` committed the previous
(pre-existing) transaction; B2/B3's "not an error" does not distinguish this.
The judged M1 observation was extended with `in_tx` at survivor acquire (IDLE in
pre/post); the 12/18 count is unchanged by that field, it only records the
state explicitly. `pg_stat_activity` was used for asyncpg (below), not for
psycopg.

Classification: **contract gap / caller obligation, not a confirmed library
bug.** psycopg has no autocommit cleanup of an explicit `BEGIN`: the caller
(the cancelled request) owns the transaction it started, and the library
provides `conn.transaction()` and `rollback()` as the tools. The public docs
fetched here were limited to search-result titles; upstream discussions
`psycopg/psycopg#555` ("Async Transaction still active after cancellation") and
`#1208` ("AsyncConnectionPool: connections can leak on CancelledError") are
known, related reports. No recovery contrast was run for psycopg direct.

## 4. asyncpg direct: cancelled transaction, reported state false then stale

K = 3: plan #1 = BEGIN round trip, #2 = INSERT, #3 = COMMIT.

* **#1 (3/3)**: cancelled in BEGIN. At that instant
  `conn.is_in_transaction() = False` and the independent `pg_stat_activity`
  view of the tested pid shows `state='idle'`, last query `SELECT 1` — the
  BEGIN is still queued client-side. The survivor's `INSERT` returns
  `('ok','INSERT 0 1')`, same-connection readback `7`, observer **`rows=[]`**.
  The queued BEGIN is flushed together with the survivor's statement, so the
  survivor's write lands inside a transaction the client does not report.
  This is the **reported-state vs actual-state divergence**: `is_in_transaction=False`,
  yet the next write is not committed.
* **#2 (3/3)**: `is_in_transaction=True`, `pg_stat_activity`:
  `state='idle in transaction'`, last query `BEGIN`. Survivor write Ok,
  readback 7, observer `[]`.
* **#3 (3/3)**: `is_in_transaction=True`, but `pg_stat_activity` shows the
  INSERT (with an assigned xid) as the last query and the observer ends up
  seeing `[['survivor',7],['target',1]]`: the target's COMMIT did execute, the
  client's Boolean was **stale**. Value-only, this is `Post` (legal); with the
  `in_tx` state field in the judged observation it is `Neither`. Value-only
  decomposition is therefore **6/9 lost-write violations** (plans #1–#2), plus
  3/9 stale-state-only trials (plan #3). Report both; do not lump them as three
  identical bugs.

Recovery check (diagnostic, plans #2/#3): after cancellation, an explicit
`ROLLBACK` returns `ok`, `is_in_transaction()` becomes `False`, and the
survivor's write is then visible to the observer (`[['survivor',7]]` and
`[['survivor',7],['target',1]]`). So the minimal caller recovery is
"ROLLBACK before reuse"; the pending-BEGIN case (#1) was not rollback-tested.

asyncpg pool (4 plans): **0/12 violations**, survivor always `in_tx=False`,
observer `[['survivor',7]]` ×6 (Pre) and `[['survivor',7],['target',1]]` ×6
(Post). The pool's reset repaired the handoff; the direct handle has no such
layer. The asyncpg pool logs `Resetting connection with an active transaction <...>`
for the released connection, consistent with the recorded `in_tx=False` at
acquire and with the 6/6 clean survivor handoffs.

Classification: **known upstream shape, reproduced on current 0.32.0** —
MagicStack/asyncpg #537 and #1310 (per the lead's notes: cancellation leaves
state unrecovered, next borrower sees "another operation is in progress"; the
cancel exception bypasses disconnect detection). This run does **not** show
"another operation is in progress": the next operation is accepted and returns
Ok while its effect is lost — a variant worth recording. The layer is the driver
/session, not SQLAlchemy #6652 (that is the pool-return variant; here the
asyncpg pool itself is clean and the direct handle is not).

## 5. aiomysql direct: sticky cancellation flag (fail-fast, then unusable)

K = 3. Every cancelled plan (#1–#3 × 3): the survivor's first use of the same
handle raises **`InterfaceError: Cancelled during execution`**; `write` and
`own` both error, `release=ok`, observer `rows=[]` ⇒ **9/9 Neither**. The
connection reports `get_transaction_status()` = 0 after plan #1 and 1 after
plans #2/#3, but that no longer matters: aiomysql marks the connection as
cancelled and refuses further work. B1/B2/B3 all 0.

This is a different shape from psycopg/asyncpg: no silent uncommitted write,
but the handle is **not reusable after a cancelled operation** (no
self-healing, no reconnect in direct mode). Classification: **unresolved /
candidate fail-fast design issue**, not triaged upstream (timebox); no patch
contrast. Pool mode for aiomysql was not run.

## 6. Triage table

| line | status | evidence | known? |
|---|---|---|---|
| psycopg pool | **clean** (positive handoff check: pool rolls back INTRANS→IDLE, same pid) | 72 trials 0 violations; M1 18/18 legal; pool log line | related upstream #1208/#555; not a defect here |
| psycopg direct | **contract gap** (manual BEGIN/COMMIT, no automatic cleanup; next user's write uncommitted, then committed/rolled back by later use) | M1 12/18 Neither; B2/B3 Ok with `status_after_acquire=INTRANS` | psycopg discussion #555 context |
| asyncpg direct | **known/reproduced** cancellation leaves a queued/open transaction; next write silently uncommitted; `is_in_transaction()` false or stale | M1 9/9 (value-only 6/9); pg_stat_activity; ROLLBACK recovery | asyncpg #537, #1310 |
| asyncpg pool | **clean** | 48 trials 0 violations; survivor in_tx False | – |
| aiomysql direct | **unresolved** sticky `InterfaceError: Cancelled during execution`; handle unusable after cancel | M1 9/9 Neither | not searched (time) |
| mysql-connector-python | **not covered** | installed only | – |

No patch contrast was produced (timebox); no upstream issue/PR/comment/email
was created. No repository code, `research/python-retro/` or Dropwise core was
modified; no commit.

## 7. Uncovered / limits

* mysql-connector-python 26.7.0 (mysql.connector.aio) and aiomysql pool mode
  were not exercised.
* No upstream page fetch: outbound `web_fetch` is blocked in this environment
  (non-public DNS); upstream facts come from search-result titles/snippets and
  the lead's notes, not from fetched issue bodies.
* One server version per engine (PostgreSQL 18.4, MariaDB 11.4); no MySQL server
  run; pool size always 1; no concurrency, TLS, or replication.
* B2/B3's "ok" is insensitive to errors the scenario itself catches into return
  values; the concrete B2/B3 values are in the logs but only M1 drives the
  verdicts.
* Recovery contrasts: only asyncpg direct ROLLBACK was checked; psycopg direct
  rollback/`transaction()` recovery and aiomysql pool were not.
* Wall clock: environment + psycopg probes/triage ≈ 55 min, asyncpg + aiomysql
  ≈ 25 min, write-up ≈ 10 min; the psycopg unit took most of the budget, so the
  4th package is a documented gap.
