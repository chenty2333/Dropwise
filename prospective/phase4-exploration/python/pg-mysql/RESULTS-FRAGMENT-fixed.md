# RESULTS-FRAGMENT — corrected psycopg / asyncpg ladders (round 3, unfrozen)

Status: **exploratory correction, 2026-10-21, not Phase 4 data.** This fragment
supersedes the psycopg 3.3.6 / asyncpg 0.32.0 B2/B3 evidence in
`RESULTS-FRAGMENT.md` and the asyncpg contrast note in
`ADAPTER-AUDIT-ROUND3.md` §2/§6. It does **not** modify `logs/run.KqRbB4/` or
any Phase 4 data. aiomysql and mysql-connector are owned by another agent and
were not edited or rerun here.

Raw logs (new, never overwritten):

| directory | content | validity |
|---|---|---|
| `logs/run.1HLY38/` | psycopg pool+direct, asyncpg pool+direct ladders, 3 reps | valid |
| `logs/run.WcLcOp/` | final boundary/recovery contrasts + 21 no-service tests | valid |
| `logs/run.nfnJcD/` | first rerun; psycopg ladders valid, **asyncpg pool ladder invalid** (released-handle diagnostic artifact, see `INVALID.md`) | partial |

Environment: CPython **3.13.13**, psycopg **3.3.6**, psycopg-pool **3.3.3**,
asyncpg **0.32.0**, PostgreSQL **18.4**, image `postgres:18-alpine` digest
`sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa`.
Loopback ephemeral port; disposable container removed afterwards. Pending:
`DelayProxy`, 50 ms per server→client chunk, tested handle/pool only; observer
direct, never through the proxy/pool. Pool size 1. Every operation bounded at
2 s; engine timeout 12 s.

## 1. Corrected per-level table

Plans = realised cut points; trials = plans × 3 reps. M1 verdicts are under
`Expect::ATOMIC` (`Pre` or `Post` only legal). 0 baseline errors in all four
units (`grep baseline_error` in `run.1HLY38/*fixed.txt` is empty).

| unit | level | yields | trials | violations | unknown | unrealized | M1 verdicts |
|---|---|---:|---:|---:|---:|---:|---|
| psycopg pool | B1 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg pool | B2 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg pool | B3 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg pool | M1 | 6 | 18 | **0** | 0 | 0 | Pre 12, Post 6 |
| psycopg direct | B1 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg direct | B2 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg direct | B3 | 6 | 18 | 0 | 0 | 0 | – |
| psycopg direct | M1 | 6 | 18 | **12** | 0 | 0 | Neither 12, Post 6 |
| asyncpg pool | B1 | 4 | 12 | 0 | 0 | 0 | – |
| asyncpg pool | B2 | 4 | 12 | 0 | 0 | 0 | – |
| asyncpg pool | B3 | 4 | 12 | 0 | 0 | 0 | – |
| asyncpg pool | M1 | 4 | 12 | **0** | 0 | 0 | Pre 6, Post 6 |
| asyncpg direct | B1 | 3 | 9 | 0 | 0 | 0 | – |
| asyncpg direct | B2 | 3 | 9 | 0 | 0 | 0 | – |
| asyncpg direct | B3 | 3 | 9 | 0 | 0 | 0 | – |
| asyncpg direct | M1 | 3 | 9 | **9** | 0 | 0 | Neither 9 |

214 ladder trials; **21 violations, 21 executions of the same two root causes**
(psycopg direct handle, asyncpg direct handle). Pool modes contribute 0.

### Old vs new delta

The counts are unchanged from `run.KqRbB4/`; what changes is evidential
validity:

| unit | old B2/B3 | new B2/B3 | old M1 | new M1 | delta |
|---|---:|---:|---:|---:|---|
| psycopg pool | 0/18 + 0/18 | 0 + 0 | 0/18 | 0/18 | counts unchanged; B2/B3 *evidence* re-established |
| psycopg direct | 0/18 + 0/18 | 0 + 0 | 12/18 | 12/18 | counts unchanged; corrected values now carry `ok/stage` |
| asyncpg pool | 0/12 + 0/12 | 0 + 0 | 0/12 | 0/12 | counts unchanged; old `obs=None` had no evidence |
| asyncpg direct | 0/9 + 0/9 | 0 + 0 | 9/9 | 9/9 | counts unchanged; old `obs=None` had no evidence |

Old B2/B3 zeros are **invalid as evidence by construction** (`_tx_write`
returned a dict without `ok` for psycopg, `None` for asyncpg; a failed stage
could not reach `level_retry`). The fixed rerun reached **0 B2/B3 stage
failures in 114 B2/B3 trials**, so the number survives — but only the rerun
supports it. Old B1/M1 counts are unaffected: B1 judged only target
outcome, and M1's survivor/observer path was independent of the swallowed
target return. `RESULTS-FRAGMENT.md` carries a correction banner pointing here.

Concrete corrected values:

- psycopg direct B2/B3: **24 of 36 retries acquired a connection in
  `INTRANS`** (`status_after_acquire=INTRANS`; plans #1–#4 × 2 levels × 3
  reps). All BEGIN/INSERT/COMMIT stage tuples are `Ok`; the retry's `COMMIT`
  ends the abandoned transaction. #5/#6 are `IDLE`.
- asyncpg direct B2/B3: **12 of 18 retries acquired with
  `is_in_transaction()=True`** (#2/#3); #1 reports `False` while a queued
  BEGIN is still client-side. All stages `Ok`.
- psycopg direct M1: survivor `write=('ok','INSERT 0 1')`,
  `own=('ok',7)`, acquire `INTRANS`, observer `rows=[]` in all 12 violations.
- asyncpg direct M1: plan #1/#2 (6 trials) observer `rows=[]` (true invisible
  write); plan #3 (3 trials) observer `[['survivor',7],['target',1]]` with
  stale acquire `in_tx=True` — **state-only Neither, not a lost write**.
  Split: **6/9 lost write + 3/9 stale-state-only**.
- psycopg pool M1: acquire `IDLE`, observer `[survivor]` (Pre, plans #1–#4)
  or `[survivor,target]` (Post, #5/#6), same backend pid.
- asyncpg pool M1: acquire `False`, observer `[survivor]` (Pre, #1/#2) or
  `[survivor,target]` (Post, #3/#4), same backend pid.

## 2. Responsibility boundary: official transaction block vs manual SQL

Fresh scenario per run, one cut per run, 50 ms proxy. `manual` = target does
`BEGIN` / `INSERT ... ON CONFLICT` / `COMMIT` as three awaits; `cm_target` =
target uses `async with conn.transaction()` (psycopg `AsyncConnection.transaction()`,
asyncpg `Connection.transaction()`); `cm_retry` = after a manual-target cut,
the **retry** writes `b3` through the official block; `rollback` = explicit
recovery before the survivor; `reset` = asyncpg `conn.reset()`;
`reconnect` = close handle/pool, reconnect, then survivor (control).
All rows below are observed in `run.WcLcOp/`; `rows` is the independent
observer after the survivor, `in_*` is the client state at acquire.

### psycopg direct (manual cut, k = 1..6)

| variant | k=1..4 | k=5..6 |
|---|---|---|
| manual (no recovery) | survivor acquire `INTRANS`, write Ok, observer `[]` | acquire `IDLE`, observer `[survivor,target]` |
| explicit `await conn.rollback()` | `status_before=INTRANS`, `rollback=Ok`, `status_after=IDLE`, observer `[survivor]` | `IDLE → IDLE`, observer `[survivor,target]` |
| retry through `conn.transaction()` (`cm_retry`) | `status_after_acquire=INTRANS`, OpResult **ok=true** (savepoint only), observer `[]` | `IDLE`, observer `[b3,survivor,target]` |
| target itself through `conn.transaction()` (`cm_target`) | k=1/2: still `INTRANS`, observer `[]`; k=3/4: `IDLE`, observer `[survivor]` | `IDLE`, observer `[survivor,target]` |
| close + reconnect | `IDLE`, observer `[survivor]` | `IDLE`, observer `[survivor,target]` |

So the official block is **not** an automatic post-cancellation recovery
guarantee: at k=1/2 its own exit did not roll back, and retrying through it on
the already-`INTRANS` handle silently commits only a savepoint. Explicit
`rollback()` and reconnect are reliable in this cut set.

### asyncpg direct (manual cut, k = 1..3)

| variant | k=1 | k=2 | k=3 |
|---|---|---|---|
| manual | acquire `in_tx=False`, observer `[]` | `True`, observer `[]` | `True` (stale), observer `[survivor,target]` |
| explicit SQL `ROLLBACK` | `in_tx_before=False → after=False`, observer `[survivor]` | `True → False`, observer `[survivor]` | `True → False`, observer `[survivor,target]` |
| `conn.reset()` | `in_tx_after=True` (!), observer `[]` — **not healed** | `False`, observer `[survivor]` | `False`, observer `[survivor,target]` |
| retry through `conn.transaction()` | OpResult ok, observer `[b3,survivor]` | OpResult **ok=false**, `InterfaceError: cannot use Connection.transaction() in a manually started transaction`, observer `[]` | same error, observer `[survivor,target]` |
| target through `conn.transaction()` | observer `[]` (still lost) | observer `[survivor]` | observer `[survivor,target]`, acquire `in_tx=True` (stale only) |
| close + reconnect | observer `[survivor]` | `[survivor]` | `[survivor,target]` |

`reset()` is not a universal repair: at k=1 (queued BEGIN, client says
`in_tx=False`) it returns Ok but leaves `in_tx=True` and the survivor write
uncommitted. Explicit SQL `ROLLBACK` heals k=1 too (it flushes/terminates the
queued BEGIN); close+reconnect is the clean control.

### Pools (positive; all contrast variants clean)

| unit | manual | rollback/reset | cm_target | cm_retry | reconnect |
|---|---|---|---|---|---|
| psycopg pool (k=1,3,5) | same pid, `IDLE`, observer `[survivor]`/`[survivor,target]` | `IDLE`, same | same pid, `IDLE`; one k=1 target error after the pool **discarded** the bad connection (`returns_bad=1`) and reconnected | `IDLE`, b3+survivor visible | new pid, clean |
| asyncpg pool (k=1..4) | same pid, `in_tx=False`, observer ok | `in_tx=False`, same | same | CM ok after pool reset, b3 visible | new pid, clean |

The psycopg pool `cm_target` k=1 target error is **pool repair, not a target
bug**: `putconn()` found a connection the cancelled transaction block left
unusable, discarded it (`returns_bad=1`), and the pool opened pid 225 for the
survivor; the observer saw the survivor. Counted as positive recovery, not a
violation.

## 3. Ordering / temporal evidence

Samples are **sequential async observations with timestamps**, not one atomic
snapshot. `seq` is emission order; `t_mono` is seconds on one monotonic clock.
The cut outcome is `cut_returned` after the target's bounded cleanup; the
cancel request itself is delivered earlier at the k-th suspension.

**psycopg direct, manual, k=1** (`run.WcLcOp/psycopg-direct-contrast.txt`):

| seq | event | observed |
|---:|---|---|
| 1 | `target_acquire` | pid 98, status `IDLE`, t=30058.744344 |
| 2 | `target_cancelled_inside` | status `INTRANS`, stages `[]`, t=30058.898076 |
| 3 | `target_before_release` | status `INTRANS` |
| 4 | `server_after_release` | state `idle in transaction`, last query `BEGIN`, t=30058.899181 |
| 5 | `target_released` | release Ok, status still `INTRANS` (direct: no reset) |
| 6 | `cut_returned` | cancelled, yields 8, realized, t=30058.899253 |
| 7 | `survivor_acquire` | same pid 98, `INTRANS` |
| 9 | `survivor_write` | `Ok('INSERT 0 1')`, status `INTRANS`, t=30059.001272 |
| 10 | `server_after_survivor_write` | `state=idle in transaction`, xid 757, query = survivor INSERT |
| 12 | `observer_read` | `[]` (survivor invisible), t=30059.104291 |

**asyncpg direct, manual, k=1** (`asyncpg-direct-contrast.txt`):

| seq | event | observed |
|---:|---|---|
| 1 | `target_acquire` | pid 245, `in_tx=False`, t=30130.371712 |
| 2 | `target_cancelled_inside` | `in_tx=False`, stages `[]` |
| 3 | `target_before_release` | `in_tx=False` |
| 4 | `server_after_release` | `state=idle`, last query is the setup `DELETE`; no BEGIN visible |
| 7 | `survivor_acquire` | same pid 245, `in_tx=False` |
| 9 | `survivor_write` | `Ok('INSERT 0 1')`, **`in_tx=True`**, t=30130.627365 |
| 10 | `server_after_survivor_write` | `state=idle in transaction`, xid 977, query = survivor INSERT |
| 12 | `observer_read` | `[]` |

Observed fact vs inference: the server sample at seq 4 does **not** show a
BEGIN; at seq 10 it shows an open transaction and the survivor's INSERT has
run. The client's `in_tx` flips from False to True only when the survivor write
is issued. “The BEGIN was queued client-side and flushed with the survivor
write” is the mechanism-consistent **inference**; the evidence is the
before/after state and query samples, not an atomic trace.

**asyncpg pool, manual, k=2** (`asyncpg-pool-contrast.txt`):

| seq | event | observed |
|---:|---|---|
| 1 | `target_acquire` | pid 307, `in_tx=False` |
| 2 | `target_stage` | `BEGIN` Ok |
| 3 | `target_cancelled_inside` | `in_tx=True` |
| 5 | `server_after_release` | `state=idle`, query = `SELECT pg_advisory_unlock_all(); CLOSE ALL; UNLISTEN *; RESET ALL;` (pool reset), t=30162.564592 |
| 6 | `target_released` | release Ok; `in_tx` read is `unavailable: InterfaceError: ... released back to the pool` |
| 8 | `survivor_acquire` | same pid 307, `in_tx=False` |
| 10 | `survivor_write` | Ok, `in_tx=False` |
| 13 | `observer_read` | `[['survivor',7]]` |

The released-handle `InterfaceError` is a **post-release diagnostic** and is
recorded as `unavailable`, never raised: the target remains `cancelled`.

**psycopg pool, manual, k=1**: `target_cancelled_inside` is `INTRANS`, then
`server_after_release` shows query `ROLLBACK`, state `idle`,
`pool_available=0→1`; survivor reacquires the same pid 195 in `IDLE` and the
observer sees `[survivor]`. This is the direct evidence for the pool positive.

### `bound()` / `wait_for` task shape

CPython 3.13.13 `asyncio.wait_for` (normal path) awaits the coroutine inside
`async with timeouts.timeout(timeout): return await fut`; despite the stale
docstring “Coroutine will be wrapped in Task”, it does **not** create a child
task. Runtime check recorded in `run.1HLY38/environment.txt`:
`outer_task=… inner_task=… child_task=False`. So `bound()` does not insert a
task boundary; the cut is delivered to the same `_CutTask`, and the engine's
yield count still measures the target's own suspensions. (The `timeout <= 0`
branch does `ensure_future`, but `BOUND=2.0` here.)

## 4. Adapter fixes and classification

Fixes in `cases/psycopg_case.py` and `cases/asyncpg_case.py` (no
`aiomysql_case.py` edit):

- `_tx_write()` checks every acquire/BEGIN/INSERT/COMMIT (and CM exit) stage;
  on a failed stage it stops before COMMIT and returns
  `OpResult(ok=False, stage=..., value=out, error=...)`; release failure is
  surfaced the same way. Cancellation is re-raised, never converted to a
  value.
- `same()`/`different()` return that `OpResult`, so `level_retry` sees it.
- Setup/magic paths raise on observer connect/create/ping, pool open/warm-up,
  and a survivor+observer **preflight**; the preflight row is deleted and the
  observer must see an empty table again, so a failed baseline cannot become
  a valid calibration and M1 is not masked.
- Diagnostics after cancellation are guarded (`_safe_pid`, `_safe_in_tx`);
  release is the first await in cleanup, so a diagnostic cannot skip it or
  turn a released pool handle into a target error. The first rerun’s
  asyncpg-pool B1/B2 false positives are filed as invalid in
  `run.nfnJcD/INVALID.md`.

Classification (same root cause per family, one target each):

| unit | classification | basis |
|---|---|---|
| psycopg pool | **pool repair positive**, not a documented guarantee | release prints “rolling back returned connection”, server query `ROLLBACK`, next acquire `IDLE`, same pid; survivor visible |
| psycopg direct | **app-composition omission / contract gap** | caller did manual `BEGIN`/`COMMIT` with no rollback/reset; docs do not promise auto-reset after cancellation; explicit rollback/reconnect repairs; official CM is not a guaranteed repair at all cuts |
| asyncpg pool | **pool repair positive**, not a documented guarantee | release prints “Resetting connection with an active transaction”, server reset query, next acquire `in_tx=False`, same pid; survivor visible |
| asyncpg direct | **driver state error (reported-state variant) + contract gap** | cancellation leaves a queued BEGIN (`in_tx=False`) or open transaction (`True`); next write succeeds but is uncommitted; known upstream shape #537/#1310; explicit ROLLBACK/reconnect repairs, `reset()` not at k=1 |
| B2/B3 layer | **contract/detection gap** | only an escaping exception/timeout is judged; transaction-state corruption after an “ok” retry is invisible. Corrected values now show 24 psycopg-direct `INTRANS` and 12 asyncpg-direct `in_tx=True` retries at 0 violations. Do not cite B2/B3 0 as semantic cleanliness. |

## 5. Tests

No-service, stdlib `unittest` (pytest not required):

- `prospective/phase4-exploration/python/pg-mysql/tests/test_adapter_outcomes.py`
  — 11 tests: failed BEGIN/INSERT stops before COMMIT, commit failure returned,
  success normalized, asyncpg no longer returns `None`, released-handle
  diagnostic does not become a target error, cancellation stays
  `CancelledError`, `OpResult(ok=False)` and timeout are B2/B3 violations.
- `research/python-retro/tests/test_ladder_outcomes.py` — lead's 10 engine
  tests (`OpResult`, dict failure, exception, timeout, setup failure).
- Run log: `logs/run.WcLcOp/tests.txt` (`Ran 11`, `Ran 10`, 21 ok).
- Related: `tests/test_adapter_contract.py` (other agent's contract tests).

## 6. Coverage / limits

- One server (PostgreSQL 18.4), loopback, pool size 1, no concurrency, TLS,
  replication, or server restart.
- Cut points are task suspensions only; 50 ms proxy makes a reply unread at
  the cut but does not cover every protocol substate. 3 reps per plan.
- Contrast plans: psycopg pool 1/3/5, asyncpg pool 1–4, direct 1–6 / 1–3;
  not every k for every variant.
- psycopg has no `Connection.reset()` in 3.3.6, so its reset contrast is
  `rollback()` only; asyncpg `reset()` was tested at three cuts.
- Official CM target changes the yield map, so contrast `k` values are not
  identical cut points to the manual ladder (noted per row).
- No upstream issue-body fetch (outbound fetch unavailable); asyncpg
  #537/#1310 and psycopg discussions #555/#1208 are search-snippet/lead-note
  level, not verified issue bodies.
- `run.nfnJcD/asyncpg-*-fixed.txt` is invalid (released-handle diagnostic);
  do not cite. `run.1HLY38` contrast files predate the release-before-sample
  reorder, so their asyncpg-pool plan-4 acquire timeout is a diagnostic
  artifact; cite `run.WcLcOp` for contrast.
- No commit was made by this agent. The lead landed commits `16f32dc` and
  `8d5290c` while this work ran; `run.KqRbB4/` mtimes are unchanged.
