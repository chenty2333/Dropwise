# RESULTS-FRAGMENT — pool ACQUIRE cancellation (psycopg_pool 3.3.3 / asyncpg 0.32.0)

Status: **exploratory phase-4 follow-up (2026-10-22), not Phase 4 data, unfrozen.**
This unit tests a mechanism different from "the next request reads a stale
reply": cancelling the **waiting acquire itself**, with pool waiter/holder
accounting as the object at risk. PostgreSQL 18.4 was a disposable loopback
container, `postgres:18-alpine`, ephemeral port, removed on exit. No commit,
push, issue, or upstream contact. Only new files were added under
`python/pg-mysql/`; no existing file or log was modified.

Canonical logs: **`logs/run.91AxzS/`** (fresh `mktemp -d`).
Code: `pool_acquire/{support,psycopg_acquire_case,asyncpg_acquire_case}.py`,
runner: `run-pool-acquire.sh`. Reused read-only: `cases/common.py` helpers and
`research/python-retro/dropwise_py/engine.py`'s `_CutTask`/`_await_target`
(the round-3 yield counter). No Dropwise core change.

## 1. Versions, image, environment

| item | value |
|---|---|
| Python | CPython **3.13.13** (`~/dropwise-py-work-r3/venv313`) |
| psycopg / psycopg_pool | **3.3.6** / **3.3.3** |
| asyncpg | **0.32.0** |
| PostgreSQL | `postgres:18-alpine`, `18.4`, digest `sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa` |
| container | `dw-pa-pg-<pid>`, `127.0.0.1:<ephemeral>`, `--rm`, removed by trap |
| workspace HEAD at run | `7197eb2` (task snapshot was `3a087df`; other agents' worktree changes were present and not touched) |

Exact commands (also in `logs/run.91AxzS/environment.txt`):

```sh
cd /home/ava/Desktop/Dropwise/prospective/phase4-exploration/python/pg-mysql
PYTHONPATH=".:/home/ava/Desktop/Dropwise/research/python-retro" \
  ~/dropwise-py-work-r3/venv313/bin/python -m pool_acquire.psycopg_acquire_case \
  --port <ephemeral> --reps 5
PYTHONPATH=".:/home/ava/Desktop/Dropwise/research/python-retro" \
  ~/dropwise-py-work-r3/venv313/bin/python -m pool_acquire.asyncpg_acquire_case \
  --port <ephemeral> --reps 5
```

The whole run is reproducible with `./run-pool-acquire.sh` (creates a new
`logs/run.XXXXXX/`, starts/removes its own container). The runner exits 0;
both case processes exit 0; `py_compile` passes. Canonical execution counts:
**35 psycopg executions + 25 asyncpg executions = 60**, of which **38 are
realised acquire cancellations**, **4 are deliberately unrealised cut probes**
(not findings), and **18 are non-cancel controls**. The 4 non-clean records
are exactly those 4 unrealised probes.

## 2. What the installed code is supposed to do (source first)

All citations are from the installed site-packages of the venv above, not from
documentation.

### psycopg_pool 3.3.3

* `AsyncConnectionPool.getconn()` increments request counters and calls
  `_getconn_with_check_loop()` (`pool_async.py:230-257`).
* `_getconn_unchecked()` takes the pool lock; if no connection is ready it
  appends a `WaitingClient` to `self._waiting` and awaits `pos.wait()`
  (`pool_async.py:290-308`).
* On a client exception (including `asyncio.CancelledError`; see
  `CLIENT_EXCEPTIONS` at `pool_async.py:38`) the handler returns
  `pos.conn` to the pool **if a connection was already assigned**, then
  re-raises (`pool_async.py:309-313`). It does **not** remove the cancelled
  `WaitingClient` from `self._waiting`; that is pruned later by
  `_add_to_pool()`, whose loop pops waiters until `WaitingClient.set()`
  accepts one (`pool_async.py:823-865`, especially `:857-862`).
* `WaitingClient.set()` refuses a client whose `error` is already set
  (`pool_async.py:985-997`), so a stale waiter cannot accept a connection and
  cannot leak it.
* Cancellation after the pop but during the configured `check` callback is
  caught by `_getconn_with_check_loop()`'s `except BaseException`, which
  returns the connection with `_putconn(..., from_getconn=True)` before
  re-raising (`pool_async.py:259-275`, especially `:268-272`).
* `max_waiting` is enforced against `len(self._waiting)` **before** a new
  waiter is queued (`pool_async.py:335-340`).

Expected: no connection loss in either cancellation position; a cancelled
queued waiter may remain observable in `_waiting`/`requests_waiting` until the
next return. The tests were designed to check exactly that, including the
`max_waiting` side effect nobody usually looks at.

### asyncpg 0.32.0

* `Pool._acquire()` awaits `self._queue.get()`, then `ch.acquire()`. The
  holder-acquire exception handler catches `(Exception, asyncio.CancelledError)`
  and **puts the holder back** into the queue before re-raising
  (`asyncpg/pool.py:994-1002`).
* `PoolConnectionHolder.acquire()` calls the user `setup` callback after the
  holder was popped. If `setup` raises or is cancelled, it closes the raw
  connection and re-raises (`asyncpg/pool.py:202-217`); `Connection.close()`
  runs `_release_on_close()`, which calls `_release()`. At setup time `_in_use`
  is still `None`, so `_release()` returns early and does **not** put the
  holder a second time; `_acquire_impl()`'s handler does the single requeue
  (`asyncpg/pool.py:343-366`).
* The min-size maintenance task may then pop the disconnected holder,
  reconnect it and put it back (`asyncpg/pool.py:542-582`). `get_size()` counts
  *connected* holders, while the holder object/queue slot still exists.

Expected: a cancelled queued acquire leaves the holder untouched; a cancelled
wake-up acquire during `setup` closes the old backend and returns the holder
to the queue (disconnected). The next acquire reconnects; no duplicate holder
and no lost queue slot.

## 3. Method (what was cut, and why the hooks)

Pool: `min_size=1, max_size=1`. Setup acquires the only connection and keeps it
checked out in the scenario instance. A second acquire is the **target**.
`survivor()` releases the holder (if still held), joins any releaser task, then
does a fresh bounded acquire + `SELECT 1` + release. `observe()` is an
independent **direct** connection with its own `application_name`; it counts
`pg_stat_activity` rows and pids for the tested application_name only (observer
excluded by `pid <> pg_backend_pid()`). No delay proxy; the acquire wait is the
suspension.

* **Shape A (queued):** holder held; target waits. Cut at its first suspension
  (`k=1`). Then the holder is released inside `survivor()` and the next acquire
  is checked.
* **Shape B (wake-up):** holder held; the target starts a releaser task
  (`sleep(0.05)` then `putconn`/`release`), waits, is woken, and the pool hands
  it the connection. A configured `check` (psycopg) / `setup` (asyncpg)
  callback gives the acquire a real suspension **after** the pop; the cut is
  `k=2`. This is cancellation at the acquirer's next suspension after the
  handoff, not during a query.
* **B handoff extra (psycopg only):** deterministic manual race. A waiter is
  queued (`len(pool._waiting)==1`), then `await pool.putconn(holder)` is called.
  In 3.3.3 this call completes without yielding to the event loop, so the
  waiter is still suspended with `pos.conn` assigned when `task.cancel()` is
  called. This exercises `pool_async.py:310-311` (`ReturnConnection`), which is
  otherwise hard to reach with `_CutTask` (a cut at `k=1` fires before the
  release; at `k=2` the acquire is already through `_getconn_unchecked`).
* **Controls:** free connection at acquire start (with/without the callback);
  normal acquire/release/reuse ×3; uncancelled waiting acquire with releaser
  (waiter must get the connection); `max_waiting=1` probe/control.

Cut harness: `_await_target(sc.target(), k, 6)` from the retro engine (the
target is a plain task; `_CutTask` cancels after its k-th suspension). The
target catches `CancelledError` only to take a synchronous stats/private-state
snapshot and immediately re-raises. Every individual operation is bounded at
2 s (pool acquire timeout, `bound(...)`, or `asyncio.timeout(2)`); scenario and
cleanup are bounded at 12 s. No `CancelledError` is converted into a value.

Private reads are read-only snapshots of `_waiting` (psycopg) and
`_holders`/`_queue` (asyncpg). No connection object is inspected after it was
returned; `safe_*_pid` exist only so a late diagnostic cannot turn into a
target failure. No `unavailable:` fallback appeared in 60 executions, no
cleanup error was recorded, every `close` is `ok`.

## 4. Per-shape / per-cut table

### psycopg_pool 3.3.3 — 35 executions

| shape | cut | reps | exec | realised | target yields | post-cancel (representative, rep1) | next acquire | final (size/avail/waiting; server) | clean |
|---|---|---:|---:|---:|---:|---|---|---|---:|
| A queued | k=1 | 5 | 5 | **5** | 1 | `requests_waiting=1`, `pool_available=0`, `_waiting=[{error:CancelledError,has_conn:false}]`, server 1 | same pid 97; `SELECT 1` ok | 1 / 1 / 0; server 1 | 5/5 |
| A queued | k=2 | 2 | 2 | 0 | 1 | no cut: target `PoolTimeout` after 2.0 s | survivor ok (same pid) | 1 / 1 / 0 | probe only |
| B check | k=2 | 5 | 5 | **5** | 2 | `check_enters=3`, `check_cancelled=true`, `pool_available=1`, `requests_waiting=0`, server 1 | same pid 111; ok | 1 / 1 / 0; server 1 | 5/5 |
| B check | k=1 | 2 | 2 | 2 | 1 | queued cut, `check` not reached; stale waiter | same pid | 1 / 1 / 0 | 2/2 |
| B handoff | manual | 5 | 5 | **5** | — | `pos_conn=true`, target not done after `putconn`; `path=pos_conn_return_task`; `pool_available` 0→1 within 1 poll (3–5 µs measured) | same pid 125; ok | 1 / 1 / 0; server 1 | 5/5 |
| free conn | k=1 + check | 3 | 3 | 3 | 1 | cancel inside `check`; connection returned (`available=1`) | same pid 141; ok | 1 / 1 / 0 | 3/3 |
| free conn | k=1, no check | 2 | 2 | 0 | 0 | acquire completed with no suspension; no cut realised | same pid | 1 / 1 / 0 | 2/2 |
| `max_waiting=1` | k=1, then new acquire | 3 | 3 | 3 | 1 | stale waiter; next `getconn` → **`TooManyRequests`** immediately (not timeout, not stale conn) | after holder return: same pid | 1 / 1 / 0 | 3/3 |
| `max_waiting=1` control | no cancel | 2 | 2 | — | — | one real waiter queued and served after release | same pid | 1 / 1 / 0 | 2/2 |
| normal | — | 3 | 3 | — | — | release, 3 × acquire/release, same pid | 3/3 pings ok | 1 / 1 / 0 | 3/3 |
| uncancelled wait | — | 3 | 3 | — | 2 | releaser hands the connection to the waiter; `outcome=completed` | same pid 151 | 1 / 1 / 0 | 3/3 |

### asyncpg 0.32.0 — 25 executions

| shape | cut | reps | exec | realised | target yields | post-cancel (representative, rep1) | next acquire | final (size/idle/qsize; server) | clean |
|---|---|---:|---:|---:|---:|---|---|---|---:|
| A queued | k=1 | 5 | 5 | **5** | 1 | holder still in use: `size=1,idle=0,qsize=0`; server 1 | same pid 167; ok | 1 / 1 / 1; server 1 | 5/5 |
| A queued | k=2 | 2 | 2 | 0 | 1 | no cut: acquire timeout at 2.0 s (engine labels the `TimeoutError` as its outer timeout) | survivor ok | 1 / 1 / 1 | probe only |
| B setup | k=2 | 5 | 5 | **5** | 3 | at target catch: `size=0,idle=0,qsize=1`, holder disconnected; 0.1 s later: `size=1,idle=1,qsize=1`, server 1 | **new pid** 181→182; ok | 1 / 1 / 1; server 1 | 5/5 |
| B setup | k=1 | 2 | 2 | 2 | 1 | queued cut; holder still in use | same pid | 1 / 1 / 1 | 2/2 |
| free conn | k=1 + setup | 3 | 3 | 3 | 2 | cancel inside `setup` after pop; old backend closed, holder requeued | new pid; ok | 1 / 1 / 1 | 3/3 |
| free conn | k=1, no setup | 2 | 2 | 0 | 0 | acquire completed with no suspension; no cut realised | same pid | 1 / 1 / 1 | 2/2 |
| normal | — | 3 | 3 | — | — | release, 3 × acquire/release, same pid | 3/3 pings ok | 1 / 1 / 1 | 3/3 |
| uncancelled wait | — | 3 | 3 | — | 2 | released holder handed to waiter; `outcome=completed` | same pid | 1 / 1 / 1 | 3/3 |

Across all 60 executions: **0 non-null top-level record `error` fields,
0 cleanup errors, 0 duplicate server backends, 0 `connections_lost`
increments, 0 subsequent acquire timeouts in the intended paths.** The only non-clean records are the
4 `k=2` Shape-A probes where no second suspension exists (the acquire waits to
its own 2 s timeout).

## 5. Concrete before/after sequences

### 5.1 psycopg Shape A (queued) — rep1

```
pre        pool_size=1 pool_available=0 requests_waiting=0  server pids=[97]
target     cancelled after 1 suspension (0.000137 s)
post       pool_size=1 pool_available=0 requests_waiting=1
           requests_queued=1 requests_errors=1
           _waiting=[{error:CancelledError, has_conn:false}]
           server pids=[97]
settled    identical after 0.1 s (the stale waiter does not go away by itself)
putconn    pool_available=1 requests_waiting=0  (stale waiter pruned by
           _add_to_pool -> WaitingClient.set() returned False)
survivor   acquired pid=97, SELECT 1 ok, released
final      pool_size=1 pool_available=1 requests_waiting=0
           server pids=[97]  (same physical backend; no extra connection)
```

The connection was never lost; the defect is only the waiter entry, see §6.3.

### 5.2 psycopg Shape B (wake-up in `check`) — rep1

```
pre        pool_size=1 pool_available=0 requests_waiting=0  server pids=[111]
release    releaser putconn(holder); requests_waiting 1 -> 0
check      3rd check enter (warm-up + holder + target); waiter has the conn
cancel     check_cancelled=true, pool_available=1, requests_waiting=0
           server pids=[111]
survivor   same pid 111; SELECT 1 ok; released
final      1 / 1 / 0; server pids=[111]
```

`_getconn_with_check_loop()`'s `except BaseException` returned the popped
connection before propagating the cancellation (`pool_async.py:268-272`).
No duplicate, no free-list hole.

### 5.3 psycopg Shape B handoff-before-resume — 5/5

```
queued       len(_waiting)=1
putconn      target still pending, pos.conn is set (pid 125), waiting emptied
cancel       CancelledError delivered before the waiter's next step
pool path    _getconn_unchecked except sees pos.conn -> run_task(ReturnConnection)
observed     pool_available=1 on first poll (3-5 us); final same pid 125
```

This is the specific defense at `pool_async.py:310-311`; without it the popped
connection would be stranded. It was reached in 5/5 deterministic races.

### 5.4 asyncpg Shape A (queued) — rep1

```
pre        size=1 idle=0 qsize=0 holders=[connected,in_use]  server [167]
target     cancelled at queue.get (1 suspension)
post       size=1 idle=0 qsize=0 (holder still checked out by setup)
putconn    size=1 idle=1 qsize=1
survivor   same pid 167; SELECT 1; release
final      size=1 idle=1 qsize=1; server [167]
```

### 5.5 asyncpg Shape B (wake-up in `setup`) — rep1

```
pre          size=1 idle=0 qsize=0; server [181]
release      holder handed to waiter; waiter pops it and enters setup
setup        cancelled at its sleep(0) suspension (target yields=3)
setup except closes raw connection 181; _release_on_close->_release returns
             early because _in_use is None; _acquire_impl requeues the holder
at catch     size=0 idle=0 qsize=1, holder disconnected; server []
post         size=0 qsize=0 (min-size maintenance has popped the disconnected
             holder to reconnect it); server 0
0.1 s later  size=1 idle=1 qsize=1; server [182]
survivor     acquires new pid 182; SELECT 1; release
final        size=1 idle=1 qsize=1; server [182]
```

The old backend is gone and exactly one new one exists. The transient
`size=0/qsize=0` is the min-size maintenance reconnect path, not a stranded
or duplicated holder; the queue never reports 2 and the server never reports 2
for this application_name.

## 6. Findings and classification

### 6.1 Primary: clean defensive positive for connection/slot accounting

In **38 realised acquire cancellations** (psycopg 23, asyncpg 15) across two
shapes, three controls, and `max_size=1`:

* no acquire that was cancelled after the pop left the connection stranded;
  psycopg returned it to the free list and asyncpg either requeued the same
  holder (queued cut) or closed/replaced it (setup-wake-up cut);
* no duplicate physical backend: `pg_stat_activity` count for the tested
  application_name stayed in `{0,1}` and ended at exactly 1, with `pool_size`
  still 1 and `pool_available`/`qsize` back to 1 after release;
* every subsequent acquire succeeded (`SELECT 1`), with the same backend pid
  for psycopg and the same pid for asyncpg Shape A; asyncpg Shape B
  deliberately used a **new** pid after the old one was closed;
* no later acquire timed out, no pool growth beyond `max_size`, no
  `connections_lost`/`returns_bad` increments;
* all 4 non-clean records are the deliberate unrealised `k=2` Shape-A probes,
  not target failures.

### 6.2 Secondary confirmed (minor) defect: psycopg_pool stale waiter

**Classification: waiter-accounting leak, not a connection leak; owner
psycopg_pool 3.3.3.** Minimal sequence (`min_size=1, max_size=1,
max_waiting=1`):

```python
pool = AsyncConnectionPool(..., min_size=1, max_size=1, max_waiting=1)
holder = await pool.getconn()                 # only connection
waiter = asyncio.create_task(pool.getconn())  # queues in _waiting
await asyncio.sleep(0)                        # let it queue
waiter.cancel(); await suppress_cancel(waiter)
# requests_waiting == 1, _waiting == [WaitingClient(error=CancelledError)]
await pool.getconn(timeout=0.5)               # -> TooManyRequests immediately,
                                              #    while no client is waiting
await pool.putconn(holder)                    # prunes the stale waiter
```

Observed 3/3: `requests_waiting=1` with a private waiter whose `error` is
`CancelledError` and `has_conn=false`; the new acquire raised
`TooManyRequests: the pool ... has already 1 requests waiting` instead of
waiting. After the holder return the next acquire succeeded with the same pid.
The cause is the exception path at `pool_async.py:309-313` not removing `pos`
from `_waiting`; `_add_to_pool` (`:857-862`) prunes it later, and
`WaitingClient.set()` (`:985-997`) correctly refuses it, so the connection
itself is safe. With the default `max_waiting=0` this is only a wrong
`requests_waiting` statistic; with a positive `max_waiting` it can briefly
reject a caller. This is neither a stale/duplicate connection nor a timeout.
It is a different layer from the round-3 "next request reads stale reply"
mechanism and should not be merged with it.

### 6.3 asyncpg observation, not a defect

Cancelling the wake-up acquire during `setup` intentionally closes the raw
connection (`pool.py:202-217`) and the holder is reconnected on the next
acquire; the 5/5 pid change is the documented fail-safe, not a lost slot. A
cancellation at the queue-get wake-up with **no** setup callback is not a
reachable cut through the public API: the holder handoff completes inside
`release()`'s shielded reset before a caller can observe/cancel the waiter at
that boundary; this unit does not simulate it with private queue injection.

## 7. Diagnostics integrity

* No connection object is read after `putconn`/`release`; pids/statuses are
  taken only while checked out. The `safe_*_pid` fallback string
  `unavailable:` appears **0 times** in the 60 executions (so the round-3
  released-connection artifact was not repeated).
* Stats snapshots are taken before/after each phase; private reads are
  read-only and exception-wrapped. A diagnostic failure would be recorded as
  a record field, not as a target failure; none occurred.
* Target outcomes are `cancelled` for the realised cuts, `completed` for the
  controls, or the expected bounded `PoolTimeout`/timeout for the 4 unrealised
  probes; no target error was produced by a diagnostic. The engine labels asyncpg's internal
  2 s acquire `TimeoutError` as `timeout after 6.0s`; raw `seconds≈2.00` and
  `yields=1` show it is the pool timeout, and that record is a probe, not a
  finding (see §8).
* Every cleanup is bounded; all `pool_close` are `ok`; no `cleanup_error`.

## 8. Limits / remaining gaps

* One event loop, one process, one PostgreSQL 18.4 on loopback; no TLS, no
  delay proxy, no server restart. Versions are exactly those above.
* `min_size=1, max_size=1` only. No `max_size>1`, no pool growth past the
  exhausted slot, no concurrent multi-waiter cancellation, no fairness/order
  test with several waiters.
* Shape B uses an explicitly configured psycopg `check` / asyncpg `setup`
  callback to create the post-pop suspension. Those are public pool knobs, but
  the measured wake-up path is the one with such a callback; without it the
  free-connection control shows the acquire can complete without any
  suspension (0 target yields), so a "cancel at wake-up" is not realised.
* psycopg's handoff-before-resume race is exercised by a deterministic manual
  harness, not by `_CutTask`; a single-cut engine cannot cancel between the
  release and the waiter's next event-loop step. The race is real 5/5 but its
  reachability in a given application depends on scheduling.
* Asyncpg's analogous handoff-before-resume cut is not reached through the
  public API without a setup callback; the result only covers the queue-get
  cut and the setup-wake-up cut.
* Single cancellation only. A second cancellation during the asynchronous
  cancellation cleanup (e.g. asyncpg's `await self._con.close()` inside the
  setup handler, psycopg's `_putconn` in `except BaseException`) was not
  injected; that is a distinct mechanism and a remaining gap.
* `max_waiting` was probed only on psycopg, 3 cancel reps + 2 control reps;
  the magnitude/practical impact for other `max_waiting` values is not
  characterized.
* 4 `k=2` Shape-A executions are unrealised by construction; they count as
  executions, not coverage of a second cut. No claim of exhaustive suspension
  coverage beyond the acquire path described.
* No upstream-novelty claim for the stale-waiter accounting wart; it is
  reported as a reproduced installed-version behavior with its layer named.
  This is exploratory evidence, not Phase 4 data, and no core/framework change
  was made.

## 9. Artifact index

| path | content |
|---|---|
| `pool_acquire/support.py` | bounded helpers, stats/private snapshots, direct observers, `safe_*_pid` |
| `pool_acquire/psycopg_acquire_case.py` | psycopg shapes/cuts/controls + max_waiting probe |
| `pool_acquire/asyncpg_acquire_case.py` | asyncpg shapes/cuts/controls |
| `run-pool-acquire.sh` | disposable PG 18.4 + both cases (new `logs/run.XXXXXX/`) |
| `logs/run.91AxzS/environment.txt` | versions/digest/commands/source line grep |
| `logs/run.91AxzS/psycopg-pool-acquire.txt` | 35 `EXEC` records + summary |
| `logs/run.91AxzS/asyncpg-pool-acquire.txt` | 25 `EXEC` records + summary |
| `logs/run.91AxzS/execution-counts.txt` | 35/33 clean, 25/23 clean, 11/8 shape rows |
