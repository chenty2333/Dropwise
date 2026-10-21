# Round-4 fragment — cancellation at transaction-context cut points

Status: unfrozen exploratory result, not Phase 4 data.  One target family
(PostgreSQL transaction context managers), direct drivers, no ORM.  New files
only; `ROUND3-RESULTS.md` is not edited. Integration corrections and targeted
reruns are recorded in [ROUND4-RESULTS.md](../../ROUND4-RESULTS.md#6-integration-review-2026-10-22).

## 0. Question

Round 3 left `cm_target` (the driver's transaction context manager) unresolved
at entry/exit.  This fragment asks a narrower question than "cancel then read a
stale reply":

> After a cancellation lands at **entry**, **body**, or **exit** of the
> transaction context manager, is the connection's actual transaction state
> consistent with (a) what the block promised, (b) whether the handle may be
> reused, and (c) the recovery the driver itself offers?

The working hypothesis was that inconsistency concentrates at a responsibility
handoff where local bookkeeping has already changed but the matching cleanup
will never run.

## 1. Environment, versions, method

| item | value |
|---|---|
| Python | CPython 3.13.13 (venv `~/dropwise-py-work-r3/venv313`) |
| psycopg | 3.3.6 (`psycopg[binary]`), libpq 18.x |
| psycopg_pool | 3.3.3 |
| asyncpg | 0.32.0 |
| server | `postgres:18-alpine`, PostgreSQL 18.4, digest `sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa` |
| network | loopback only; tested connection through `DelayProxy` (50 ms per server→client chunk); independent observer direct, never through the proxy |
| run | `logs/run.YUPlqK/` (authoritative), earlier `run.BCM100/`, `run.m1b.QotH50/` |
| commands | `./run-txphase.sh`; `./run-pool-txphase.sh` (pool layer); `python -m unittest tests.test_txphase` |

The engine is the read-only Python port (`dropwise_py`).  A `_PhasedTask`
subclasses `PureTask` (not `_CutTask`, to avoid double counting) and records the
scenario's own phase label when the cut is requested.  A cut is counted only
when the task requested it and the outcome is `cancelled` / `error` /
`timeout`; unrealised plans are reported, not interpreted.

Phase labels come from the scenario (`enter`, `body`, `exit_rollback`,
`exit_commit`), not from a wire trace.  They were cross-checked against the
source paths below and against the server's `pg_stat_activity` state.

Table shape: `(k text primary key, v integer not null)`; observer reads only
committed rows.  Each trial has a unique table.  Values are distinct enough
that a stale row cannot be mistaken for the retry's row (target=1, retry=5,
survivor=7, post=9).

## 2. What is actually promised

**psycopg 3.3.6.**  `AsyncConnection.transaction()` is an
`@asynccontextmanager` (`connection_async.py:383-403`).  Its body does
`async with tx:` and yields the internal `AsyncTransaction`.  The docs say an
exception inside the block rolls the transaction back and that
`Connection.rollback()` is not to be called inside a transaction block.  The
docs do **not** say what happens if the task is cancelled while `__aenter__` is
still running.  Python's `async with` does not call `__aexit__` when
`__aenter__` raises, so that case has no block cleanup by construction.

**asyncpg 0.32.0.**  `Connection.transaction()` returns a `Transaction`; its
async context manager rolls back on an exception raised *in the block*
(`transaction.py:63-94`).  `Transaction.start()` sets `_top_xact` before
`BEGIN` (`transaction.py:96-151`).  The docs do not cover cancellation during
`start()` either.

So the single-cancel entry results below are a **contract gap / implementation
defect candidate**, not a violation of an explicit documented promise.  The
silent part (a retry reporting success while the write is invisible) is the
part with user-visible consequence.

## 3. Phase map, single cancellation, no repair

`k` is the cut index in the engine's yield count.  `retry` is a fresh
`_cm_write` on the same handle after a 150 ms settle; `rows` is the independent
observer after the retry.  `tx_status` is `None` for psycopg because the
public `conn.transaction()` result is the context manager, not the internal
`AsyncTransaction`; that inaccessibility is itself part of the finding.

| driver | k | recorded phase | client state after unwind | server for tested pid | retry | observer rows |
|---|---:|---|---|---|---|---|
| psycopg | 1 | enter | `num_transactions=1`, INTRANS | idle in transaction (`BEGIN`) | ok | `[]` |
| psycopg | 2 | enter | `num_transactions=1`, INTRANS | idle in transaction (`BEGIN`) | ok | `[]` |
| psycopg | 3 | body | `num_transactions=0`, IDLE | idle (`ROLLBACK`) | ok | `[retry,5]` |
| psycopg | 4 | body | `num_transactions=0`, IDLE | idle (`ROLLBACK`) | ok | `[retry,5]` |
| psycopg | 5 | exit_commit | `num_transactions=0`, IDLE | idle (`COMMIT`) | ok | `[retry,5],[target,1]` |
| psycopg | 6 | exit_commit | `num_transactions=0`, IDLE | idle (`COMMIT`) | ok | `[retry,5],[target,1]` |
| asyncpg | 1 | enter | `in_transaction=True`, `_top_xact=tx`, FAILED | idle in transaction (`BEGIN`) | ok | `[]` |
| asyncpg | 2 | body | `in_transaction=False`, ROLLEDBACK | idle (`ROLLBACK`) | ok | `[retry,5]` |
| asyncpg | 3 | exit_commit | `in_transaction=False`, FAILED | idle (`COMMIT`) | ok (150 ms settle) | `[retry,5],[target,1]` |

Read: entry cuts leave an open server transaction and a retry that **succeeds
without the write becoming visible**.  Body cuts roll back and the retry is
visible.  Exit-commit cuts may have committed; the retry is then visible on a
new transaction, but the cancelled commit's outcome is inherently ambiguous.

## 4. Entry-cut mechanism

### 4.1 psycopg

1. `AsyncTransaction._enter_gen()` sets `status = ACTIVE` and calls
   `_push_savepoint()` **before** sending `BEGIN`
   (`transaction.py:99-106`).
2. `_push_savepoint()` decides outer vs inner from
   `pgconn.transaction_status == IDLE`, asserts there are no open transaction
   blocks for an outer one, and increments `_conn._num_transactions`
   (`transaction.py:209-227`).
3. The `BEGIN` command blocks in `AsyncConnection.wait()`.  On
   `asyncio.CancelledError`, `wait()` (only if the protocol is `ACTIVE`) sends
   `cancel_safe`, drains the command for up to 5 s, and re-raises
   (`connection_async.py:516-548`).  So the server ends in `idle in
   transaction`, the stream is aligned, and the driver's own cancellation
   handling looks correct at the socket level.
4. The cancellation propagates out of `__aenter__`.  `async with` never calls
   `__aexit__`; the internal `AsyncTransaction` remains `ACTIVE`; the local
   counter remains 1.  The public context-manager object never yielded the
   transaction, so the caller has no `tx.rollback()` to call.
5. `conn.rollback()` is rejected by the driver's own guard:
   `_num_transactions` is nonzero (`_connection_base.py:597-604`), raising
   `ProgrammingError: Explicit rollback() forbidden within a Transaction
   context. (...)`.  This is not a server error; the server is healthy.
6. A natural retry `async with conn.transaction(): ...` calls
   `_push_savepoint()` again.  Because the server is `INTRANS`, it takes the
   **inner** branch, creates `_pg3_2`, issues `SAVEPOINT`, and its `__aexit__`
   issues `RELEASE`.  The write is inside the abandoned outer transaction.
   The retry returns success, `_num_transactions` returns to 1, and an
   independent observer sees nothing (table above).
7. If the caller instead tries raw SQL `ROLLBACK` (bypassing `conn.rollback`'s
   guard), the server returns to IDLE but `_num_transactions` stays 1.  The
   next `conn.transaction()` computes `_outer_transaction=True`, then asserts
   `not self._conn._num_transactions` and raises a bare **`AssertionError`**
   (`transaction.py:219`).  This is a second, avoidable dead end.  (Under
   `python -O` the assert disappears and the client would send `BEGIN` while
   the local stack still has the stale entry; untested.)

Thus the connection is left in a state where the driver's documented
transaction API cannot recover it, while `conn.info.transaction_status` is
`INTRANS` and `conn.closed` is false.  Only `close()` / reconnect (or a
driver-level repair) recovers.

### 4.2 asyncpg

1. `Transaction.start()` sets `con._top_xact = self` before
   `await con.execute('BEGIN')` (`transaction.py:105-110`).
2. Cancellation of that execute sets `self._state = FAILED`
   (`transaction.py:144-151`) but does **not** clear `_top_xact`.
3. A retry `conn.transaction().start()` sees `_top_xact is not None`, takes the
   nested branch, and issues `SAVEPOINT` (`transaction.py:112-130`).  The
   server is still `INTRANS`, so the savepoint succeeds, the retry's
   `__aexit__` issues `RELEASE`, and the write is invisible.
4. `Transaction.rollback()` cannot help: `__check_state()` rejects `FAILED`
   with `InterfaceError: cannot rollback; the transaction is in error state`
   (`transaction.py:153-173`).
5. SQL `ROLLBACK` (or `conn.execute("ROLLBACK")`) ends the server transaction
   but leaves `_top_xact` set; the next `conn.transaction()` then emits
   `SAVEPOINT` against an idle server and fails with
   `NoActiveSQLTransactionError: SAVEPOINT can only be used in transaction
   blocks`.
6. `conn.reset()` is the public recovery that works: `_reset()` clears
   `_top_xact` and sends `ROLLBACK` (`connection.py:1550-1565`).  Reconnect
   also works.

At the ORM layer the same shape is known: SQLAlchemy issue #13381 (closed)
reports a cancelled implicit `BEGIN` leaving `_top_xact` set, the next checkout
seeing a top-level transaction, and rollback/commit only affecting the
savepoint (`asyncpg connection returned to pool with open transaction...`).
SQLAlchemy closed it by wrapping event-hook execution in error handling, not by
changing asyncpg.  So this is a **known mechanism instance at a higher layer**,
new here at the direct driver API with an independent observer and a recovery
matrix.

## 5. Recovery matrix (entry cuts, k=1 and k=2, 2/2 each)

| driver | attempted recovery | immediate result | next CM retry | observer |
|---|---|---|---|---|
| psycopg | none | — | ok | `[]` |
| psycopg | `conn.rollback()` | `ProgrammingError` (guard) | ok | `[]` |
| psycopg | raw `ROLLBACK` | server idle | `AssertionError` in next `_push_savepoint` | `[]` |
| psycopg | `close()` + reconnect | new backend | ok | `[retry,5]` |
| psycopg | experimental `--fix-enter` | entry rolls back | ok | `[retry,5]` |
| asyncpg | none | — | ok | `[]` |
| asyncpg | SQL `ROLLBACK` | server idle, `_top_xact` stale | `NoActiveSQLTransactionError` | `[]` |
| asyncpg | `Transaction.rollback()` | `InterfaceError` (FAILED) | not reached | — |
| asyncpg | `conn.reset()` | `_top_xact` cleared | ok | `[retry,5]` |
| asyncpg | reconnect | new backend | ok | `[retry,5]` |
| asyncpg | experimental `--fix-enter` (terminate) | connection closed | `InterfaceError: connection is closed` | `[]` |

The psycopg `--fix-enter` variant is a runtime monkeypatch of
`AsyncTransaction.__aenter__`: on a `BaseException` after the status became
`ACTIVE`, it acquires the connection lock and runs `_rollback_gen(exc)`, then
re-raises.  It was validated by the phase probe (2/2: IDLE, `_num_transactions`
0, retry visible) and by M1 (0/12 violations); pre/post calibration in that run
also exercises the unpatched normal path.  It is a **feasibility
demonstration**, not upstream code, and is only active under `--fix-enter`.

The asyncpg fail-closed variant terminates the connection; it prevents a
silently nestable transaction but does not repair the handle.  A rollback
attempted *inside* the cancelled `__aenter__` was not reliable: the
cancellation machinery can still be in flight and the `BEGIN` reply may not
have been consumed, so a ROLLBACK can be matched against stale protocol state
(the direct experiment produced an `InterfaceError: cannot use
Connection.transaction() in a manually started transaction` on the next
attempt).  `reset()` after the task has unwound is reliable; rollback from
inside the entry handler is not.

## 6. Ladders (fixed harness, authoritative run)

`run.YUPlqK`, 3 repetitions per cut.  psycopg has 6 cuts (2 enter, 2 body,
2 exit-commit); asyncpg has 3 cuts (1 per phase).

| level | psycopg | asyncpg | what is checked |
|---|---|---|---|
| B1 cancel-only | 18 trials, 0 violations | 9, 0 | no escaped error/timeout/leak |
| B2 same-statement retry | 18, 0 | 9, **3** (plan #3) | retry must not raise/fail |
| B3 different-statement retry | 18, 0 | 9, **3** (plan #3) | same |
| M1 survivor + observer | 18, **6 Neither** (#1,#2) | 9, **6 Neither** (#1,#3) | survivor effect must be Pre or Post |
| M1 + reconnect (psycopg) / reset (asyncpg) | 12, 0 | 6, 0 | recovery contrast |
| M1 + `--fix-enter` (psycopg) | 12, 0 | — | driver repair contrast |

M1 verdict distribution:
psycopg `none` = 6 Pre + 6 Post + 6 Neither; psycopg reconnect/fix = 8 Pre +
4 Post, 0 Neither.  asyncpg `none` = 3 Pre + 6 Neither; asyncpg reset = 4 Pre
+ 2 Post, 0 Neither.

Two method points follow directly:

* **B2/B3 cannot see the psycopg entry trap.**  The nested retry returns `ok`,
  so a retry-level check reports 0/18 while M1 reports 6/18.  This is the same
  observation-completeness issue as round 3, now with a concrete mechanism.
* **asyncpg B2/B3 see plan #3, not plan #1.**  At exit-commit the immediate
  retry raises `InterfaceError: cannot use Connection.transaction() in a
  manually started transaction` (3/3 B2 and 3/3 B3).  A probe with a 150 ms
  settle succeeds, so this is a **fail-fast race**, not a stable failure: the
  cancel leaves the protocol's transaction status stale for a short window
  while `_top_xact` was already cleared by `__commit`
  (`transaction.py:175-190`).  M1's #3 is `Neither` because the survivor
  cannot run and the target row may or may not have committed.

## 7. Double cancellation (cancel again at the first suspension after the cut)

This is a separate model (a supervisor cancelling twice); it is not the
single-cancel result.

| driver | k | first phase | second phase | client after | server | next retry |
|---|---:|---|---|---|---|---|
| psycopg | 1,2 | enter | enter | `num_transactions=1`, ACTIVE | idle in transaction | `OperationalError: another command is already in progress` |
| psycopg | 3,4 | body | body | 0, ACTIVE | idle in transaction | same |
| psycopg | 5,6 | exit_commit | exit_commit | 0, ACTIVE | idle (target committed) | same |
| asyncpg | 2 | body | exit_rollback | `in_transaction=True`, `_top_xact=False`, FAILED | idle in transaction | `InterfaceError: cannot use Connection.transaction() in a manually started transaction` |
| asyncpg | 1 | enter | — | `_top_xact=tx`, FAILED | idle in transaction | ok, rows `[]` |
| asyncpg | 3 | exit_commit | — | FAILED, IDLE | idle (target committed) | ok, target+retry visible |

psycopg 1–4 leave the transaction open (and the connection stuck in `ACTIVE`);
5–6 have committed the target but the connection is still stuck.  This matches
open psycopg issue #1444 ("A second Ctrl-C during `Connection.wait()`'s cancel
cleanup leaves the connection busy") and its proposed fix PR #1448 (close the
connection if interrupt arrives during cancel cleanup).  It is a known
repeated-interrupt hazard, not a new root cause.  asyncpg's second cancel
during rollback leaves `_top_xact` cleared but the server `INTRANS` and the
protocol thinking a transaction is open; no successful retry.

## 8. Pool layer (psycopg_pool 3.3.3 / asyncpg 0.32.0 pools)

The pool layer is where the entry defect meets a real ownership boundary.
`run.pool.Fgn9iC`, 3/3 identical per cell.

psycopg_pool `_reset_connection()` (`pool_async.py:871-899`): if the returned
connection is `INTRANS`, it calls `conn.rollback()`.  The driver guard raises;
the pool catches `CLIENT_EXCEPTIONS` and closes/discards the connection.

| caller behaviour | what the caller sees | pool action | next checkout |
|---|---|---|---|
| baseline | success, target visible | reuse | same pid, clean |
| propagate cancellation | `CancelledError` | 4 warnings; discard | new pid, clean |
| swallow cancellation, exit normally | `ProgrammingError: Explicit commit() forbidden...` | discard | new pid, clean |
| retry inside the same checkout | retry `ok`, observer `[]` | discard after the block | new pid, clean |

asyncpg pool releases through `Connection.reset()`/`_reset()`
(`pool.py:255-280`): the open transaction is rolled back and the connection is
reused.

| caller behaviour | what the caller sees | pool action | next checkout |
|---|---|---|---|
| baseline | success, target visible | reuse | same pid, clean |
| propagate cancellation | `CancelledError` | reset; rollback | same pid, clean |
| swallow cancellation | no error | reset; rollback | same pid, clean |
| retry inside the same checkout | retry `ok`, observer `[]` | reset after the block | same pid, clean |

Interpretation:

* The pool layer **does restore reuse qualification across checkouts** — this
  is a real defensive positive, by two different mechanisms (discard vs
  reset).
* It **cannot prevent the in-block false success**: the retry reports `ok`
  while the observer sees nothing, and the pool only cleans up when the block
  ends.  A handler that returns a 200 inside that block has already lied.
* psycopg_pool also converts "swallowed cancellation, normal exit" into a
  `ProgrammingError` at block exit, because `AsyncConnection.__aexit__` tries
  to `commit()` and hits the same `_num_transactions` guard.  That is
  fail-loudly but surprising; the caller did not ask to commit.
* If the caller retries inside the same checkout, both pools are irrelevant
  until release.

## 9. Known vs new (read-only search)

* SQLAlchemy #13381 (closed, 2026-06-17): asyncpg connection returned to the
  pool with an open transaction after cancellation inside
  `before_cursor_execute`; later reuse sees `_top_xact` and emits `SAVEPOINT`.
  Same mechanism as §4.2.  SQLAlchemy's resolution was to wrap event hooks in
  error handling; the asyncpg bookkeeping was not changed.
* psycopg #1444 (open, 2026-10-16) and PR #1448 (open): second interruption
  during `wait()`'s cancel cleanup leaves the connection `ACTIVE`; matches §7.
* psycopg single-cancel transaction-entry trap: issue search for
  `forbidden within a Transaction`, cancellation, and transaction keywords did
  not find a matching report.  This is "not found, not triaged", not a claim
  of novelty.  Documentation pages also do not mention the entry case.
* psycopg docs: <https://www.psycopg.org/psycopg3/docs/basic/transactions.html>;
  asyncpg transaction API: <https://magicstack.github.io/asyncpg/current/api/index.html#asyncpg.connection.Connection.transaction>.

## 10. Classification

| observation | classification |
|---|---|
| psycopg entry cut: server INTRANS, `_num_transactions=1`, `rollback()` rejected, natural retry silently nests and leaves the write uncommitted | **defect candidate / contract gap**; unexpected transaction scope and invisible retry write, not proof of permanent data loss |
| psycopg raw `ROLLBACK` then next CM `AssertionError` | **confirmed second dead end**; follows from the same bookkeeping gap |
| psycopg low-level cancellation handling (cancel + drain) | defensive positive at the socket level; the failure is above it, in the transaction object's bookkeeping |
| asyncpg entry cut: `_top_xact` stays set, nested savepoint, silent retry | same shape; **known at ORM layer (#13381)**, direct-driver reproduction here; `reset()`/reconnect are working recoveries |
| asyncpg exit-commit immediate retry: `InterfaceError` | **fail-fast race** (150 ms settle removes it); not silent, not stable |
| psycopg/asyncpg body cut: rollback, retry visible | **defense positive**, allowed outcome |
| exit-commit psycopg: target+retry visible | allowed ambiguous outcome; commit fate is inherently unknowable to the client |
| pool discard / pool reset | **layer defense positive**; restores reuse qualification, not in-block truth |
| repeated cancellation | known hard case (#1444); distinct from single cancel |

The working hypothesis is supported for **entry-phase** cancellation and needs
a sharper form: the high-value commonality is not "cancel then a stale response
is read" but

> cancellation after local bookkeeping has been mutated and before the
> matching `async with` cleanup is guaranteed to run, followed by a retry that
> silently nests inside the abandoned state.

That is why the psycopg retry succeeds but the observer sees nothing.  The
hypothesis does **not** generalise to body cuts (clean rollback) or to
commit-phase cuts (ambiguous by nature; asyncpg's immediate retry fails
loudly).

## 11. Limits

* One PostgreSQL image/version; one table shape; database owner; no TLS,
  pgbouncer, hot standby, or network faults beyond the 50 ms proxy.
* Direct driver only (no SQLAlchemy/ORM); pool layer only as in §8, size 1.
* One cut per trial except the explicit double-cancel series; 2 s per
  operation, 12 s per scenario.
* Phase labels are scenario labels, not a protocol trace; the server state and
  source paths corroborate them.
* psycopg's internal `AsyncTransaction` is not public after a failed entry, so
  `tx_status` is unavailable; `conn.info.transaction_status` is used.
* The asyncpg commit-cut race was sampled, not bounded; 3/3 immediate failures
  and 1/1 success after 150 ms is a direction, not a distribution.
* The `--fix-enter` variants are runtime monkeypatches for contrast; they are
  not upstream patches and were never active for the original measurements.
* The psycopg `raw ROLLBACK` consequence under `python -O` was not tested.

## 12. Raw evidence

* `logs/run.YUPlqK/` — authoritative: environment, harness tests, counts,
  phase probes, recovery probes, double-cancel probes, B1/B2/B3/M1 ladders,
  recovery and fix ladders.
* `logs/run.m1b.QotH50/` — first clean M1 after removing the volatile backend
  pid from the judged observation (superseded by `run.YUPlqK` for counts).
* `logs/run.pool.Fgn9iC/` — pool-layer 4×3 per driver.
* `logs/run.BCM100/` — earlier run before the pid/recovery-metadata harness
  fix; its M1 numbers are `Unknown` and are **not** used.

The harness changes made during this round are themselves documented:
`_PhasedTask` must subclass `PureTask` (subclassing `_CutTask` double-counts and
double-cancels), the backend pid and recovery diagnostics must not be part of
the judged observation (they differ for harness reasons), and `--probe-phase`
plans beyond the realised K are unrealised and excluded.
