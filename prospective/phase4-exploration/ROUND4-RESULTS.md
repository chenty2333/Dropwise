# Round-4 mainline results — cancellation at ownership handoffs

Date: 2026-10-22 (Asia/Tokyo).  Base: `main` at `7197eb2` (the Linux patch
task committed while this round ran).  This file is a round-4 summary, not
Phase 4 data and not a frozen preregistration.  It does not edit or supersede
`ROUND3-RESULTS.md`; where a round-3 entry was unresolved, the new result is
stated as a resolution, not appended as a contradiction.

Scope of this round: one deliberately broad pass over **transaction context
entry/body/exit**, **pool return/acquire ownership**, and **two protocol
clients** (mysql-connector-python aio, sqlx direct `PgConnection`).  Linux
multicast and Skytable belong to the parallel patch task and were not touched,
rerun, or edited.

## 0. What actually ran

| target | executions (cut trials unless stated) | authoritative logs | new files |
|---|---:|---|---|
| psycopg 3.3.6 direct CM | 72 ladder + 24 recovery/fix + 21 probes/count | `python/pg-mysql/logs/run.YUPlqK/` | `cases/txphase_case.py`, `tests/test_txphase.py`, `run-txphase.sh` |
| asyncpg 0.32.0 direct CM | 36 ladder + 6 reset + 14 probes/count | same | same |
| psycopg_pool 3.3.3 / asyncpg pool | 24 pool trials (4 caller behaviours × 3 reps × 2 drivers) | `python/pg-mysql/logs/run.pool.Fgn9iC/` | `cases/pool_txphase_case.py`, `run-pool-txphase.sh` |
| mysql-connector-python 26.7.0 aio | 194 logged executions + 16 calibration scenarios | `python/pg-mysql/logs/run.zp7wH6/`, `run.1fMJtP/`, `run.gTlHvk/` | `cases/mysqlconnector_recovery_case.py`, `fix/patched/`, `RESULTS-FRAGMENT-recovery.md` |
| psycopg_pool / asyncpg pool acquire | 60 executions (35 psycopg, 25 asyncpg) | `python/pg-mysql/logs/run.91AxzS/` | `pool_acquire/`, `RESULTS-FRAGMENT-pool-acquire.md` |
| sqlx 0.8.6 direct `PgConnection` | 48 cut + 12 baselines | `clients/sqlx/logs/run.Eycpui/` | `clients/sqlx/` + fragment |

The PostgreSQL runners used disposable `postgres:18-alpine`; the
mysql-connector runners used disposable `mysql:9.6`. Both bind loopback and
write to fresh log directories, with independent observers bypassing the delay
proxy.  No existing log was overwritten.  No core engine/runner API was added.

## 1. Most important new facts vs the previous results

1. **`cm_target` is resolved, and the entry phase is the defect.**  Round 3
   reported psycopg `cm_target` as INTRANS at k=1/2 with observer `[]`, and
   left the mechanism open.  The phase-aware probe shows the cancellation is
   in `AsyncTransaction.__aenter__` *before the block is entered*.  psycopg
   has already set the transaction ACTIVE and incremented `_num_transactions`
   before sending `BEGIN`; the `BEGIN` reply is drained by the driver's
   cancellation handler, so the **server is healthy and INTRANS**, but the
   async-with cleanup will never run.  `conn.rollback()` is then rejected by
   the driver's own guard; a natural retry nests as a SAVEPOINT and reports
   success while an independent observer sees nothing. The retry write remains
   uncommitted in the abandoned outer transaction;
   this is not evidence of permanent data loss, nor of "driver rollback failed
   inside the block".

2. **asyncpg has the same shape at entry, with a working public recovery.**
   `Transaction.start()` sets `_top_xact` before `BEGIN`; cancellation leaves
   `_top_xact` set and `_state=FAILED`.  The next `conn.transaction()` becomes
   a nested SAVEPOINT, reports success, observer sees nothing.  `reset()` and
   reconnect clear it; SQL `ROLLBACK` does **not** (the next CM fails with
   `NoActiveSQLTransactionError`).  The same nested-savepoint symptom is
   already reported at the SQLAlchemy layer (issue #13381, closed); this is a
   direct-driver reproduction of a known mechanism, not a new root cause.

3. **Body cuts are clean; exit-commit cuts are not a silent defect but asyncpg
   has a fail-fast race.**  psycopg k=3/4 and asyncpg k=2 roll back, leave
   `IDLE`, and the retry's write is visible; M1 verdicts are Pre.  At
   exit-commit, psycopg's target and retry are both visible (Post/Pre allowed).
   asyncpg's immediate retry after a cancelled COMMIT fails with
   `InterfaceError: cannot use Connection.transaction() in a manually started
   transaction` 3/3 in B2/B3, while the same probe with a 150 ms settle
   succeeds; the protocol's transaction status is stale for a short window
   after `_top_xact` was cleared.  Classify as **fail-fast race**, not silent
   loss.

4. **The reported survivor+observer executions expose the entry trap.**  psycopg B2/B3 are
   0/18 violations while M1 is 6/18 Neither (#1/#2 × 3).  asyncpg B2/B3 catch
   only the exit-commit race (3/9, plan #3), not the entry silent path; M1
   catches entry (#1) and exit (#3) = 6/9. These separate executions show why a
   retry-only error check can miss
   the effect mismatch. They are not a same-trajectory paired ablation or a
   detection-rate estimate.

5. **mysql-connector's old M1 `Neither` now has a recovery contrast and a
   fix.**  The cancelled command leaves its reply unread; `unread_result` is
   false and the socket is still open.  `rollback()` returned OK 12/12 but
   recovered 0/12; `reset_session()` returned OK 9/12 and recovered 0/12;
   21/24 official recovery calls were false successes and 18/36 returned
   silently wrong user-visible values (cut 3/4: `rollback()` OK, survivor
   committed, client saw rowcount 0 / empty readback; cut 1: the user SELECT
   returned the connector's internal `SELECT @@session.sql_mode` string).
   Of the tested strategies, `close()` + reconnect restores usable behaviour
   (12/12).  A patched copy
   (socket-layer cancel poison + `handle_unread_result()` refusal, with a
   `task.cancelling()` guard) preserves `CancelledError` in B1 0/12 and makes
   the tested later commands fail loudly; it is fail-closed containment, not repair.

6. **sqlx is a mature-defense counterexample.**  Direct `PgConnection` 0.8.6,
   48 cuts / 12 baselines, 0 violations: `wait_until_ready()` plus
   `pending_ready_for_query_count` drains the abandoned exchange through
   `ReadyForQuery` before the next send.  In the strongest trial the target was
   applied server-side, 32 bytes of its reply were still unread in the client
   socket, and the next write returned its own rowcount 4, not the target's 3.
   The connection stayed usable and `proxy_connections=1`.  This is the same
   "unfinished exchange" shape as the framed clients, ending in a drain instead
   of a stale association.

7. **Pool return is a layer defense, not a cure.**  psycopg_pool 3.3.3 sees the
   returned connection INTRANS, calls `conn.rollback()`, gets the driver guard
   error, and discards the connection (3/3; four warnings; new backend on the
   next checkout).  asyncpg's pool calls `reset()` and reuses the same backend
   (3/3).  Both prevent cross-checkout contamination; neither prevents a retry
   *inside the same checkout* from reporting success while the observer sees
   nothing.  psycopg_pool additionally turns "swallow the cancellation and exit
   normally" into `ProgrammingError: Explicit commit() forbidden...` at block
   exit.

8. **Repeated cancellation is a known hard case, not a new root cause.**
   psycopg second-cancel leaves the connection `ACTIVE` and every later
   statement fails with `another command is already in progress` (matches open
   psycopg #1444 / PR #1448).  asyncpg second-cancel during rollback leaves
   `_top_xact` cleared but the server INTRANS and the protocol thinking a
   transaction is open; no successful retry.

9. **Pool acquire is clean for connection/slot accounting.**  60 executions:
   38 realised acquire cancellations, 18 controls, 4 deliberately unrealised
   probes.  No leaked or stranded connection/slot, no duplicate backends, no
   `connections_lost`.  One minor confirmed psycopg_pool 3.3.3 waiter-accounting
   defect: a cancelled queued waiter stays in `_waiting` (`requests_waiting=1`,
   no conn) until the next `putconn`; with `max_waiting=1` the next `getconn`
   raises `TooManyRequests` 3/3.  Connection accounting is a defensive
   positive; the waiter statistic is not.

10. **Double-cancellation and commit-phase outcomes must not be folded into the
    entry finding.**  They are different mechanisms with different caller
    obligations: entry needs close/reset; body is already safe; exit commit is
    an inherently ambiguous outcome (or a short fail-fast window), not a
    promise that the client can resolve.

## 2. Key targets: contract, coverage, mechanism, contrast

### 2.1 psycopg 3.3.6 async transaction entry

* **Normal path, covered:** preflight CM write + independent observer;
  `B1` 18 trials, 0 violations; `count_yields` 6; no escaped error.
* **Contract:** docs promise rollback for an exception *inside* the block and
  forbid `conn.rollback()` inside a transaction block.  They do not say what
  happens when `__aenter__` is cancelled; `async with` does not call
  `__aexit__` if `__aenter__` raises.  This is a contract gap, not a violated
  sentence.
* **Reached intervention:** k=1/2 is inside `_enter_gen` after
  `_push_savepoint()`; server `idle in transaction`, `num_transactions=1`,
  `status=INTRANS`; retry `ok`, observer `[]`.
* **Mechanism:** `_enter_gen` ACTIVE + `_push_savepoint` before `BEGIN`
  (`transaction.py:99-106`, `209-227`); cancellation drained by
  `wait()` (`connection_async.py:516-548`); `conn.rollback()` guard
  (`_connection_base.py:597-604`); retry takes the inner/savepoint branch
  because `transaction_status != IDLE`.
* **Contrast:** `rollback()` → `ProgrammingError`; raw `ROLLBACK` → server IDLE
  but next `conn.transaction()` raises a bare `AssertionError`; reconnect or
  the experimental `__aenter__` repair → retry visible; M1 6/18 → 0/12 with
  repair.
* **Defect candidate, not proven upstream:** no matching psycopg issue was
  found in the searched tracker; this is "not found, not triaged", and the
  docs are silent.  The silent retry success is the part that matters.

### 2.2 asyncpg 0.32.0 transaction entry and exit

* **Normal path, covered:** preflight, B1 9 trials / 0 violations, count 3.
* **Contract:** transaction CM rolls back on an exception in the block; the
  docs do not cover cancellation during `start()`.
* **Reached intervention:** k=1 `start()` after `_top_xact=self`, before/at
  `BEGIN`; `in_transaction=True`, `_top_xact=tx`, `FAILED`; retry `ok`,
  observer `[]`.
* **Mechanism:** `_top_xact` before `BEGIN` (`transaction.py:105-110`);
  cancellation sets FAILED but not clear `_top_xact` (`:144-151`); next
  `start()` takes the nested SAVEPOINT branch (`:112-130`); `rollback()`
  rejects FAILED (`:153-173`); `reset()` clears and rolls back
  (`connection.py:1550-1565`).
* **Contrast:** `reset()`/reconnect → retry visible; SQL `ROLLBACK` → next CM
  `NoActiveSQLTransactionError`; immediate retry after exit-commit →
  `InterfaceError` 3/3, 150 ms settle → success (race).
* **Known:** SQLAlchemy #13381 has the same nested-savepoint symptom.

### 2.3 mysql-connector-python 26.7.0 aio

* **Normal path, covered:** preflight and uncancelled calibration per build.
* **Contract:** `start_transaction(...)` raises "Transaction already in
  progress" (documented fail-fast) and is excluded from the silent finding.
  The connector does not document reuse after a cancelled command.
* **Reached intervention:** four realised cuts; the cancelled command's reply
  stays in the stream, `unread_result` false, socket open.
* **Mechanism:** reply remains queued; `rollback()` / `reset_session()` send a
  new command on the same stream, so the next command consumes the old reply.
  At cut 3/4 the survivor's REPLACE executed and committed (observer saw
  `survivor,7`) while the client reported rowcount 0 and an empty readback; at
  cut 1 the user SELECT returned the internal `@@session.sql_mode` string.
* **Contrast:** official recovery false-success 21/24; close+reconnect 12/12;
  patched copy B1 0/12, tested later commands loud; patched M1/B2/B3 unchanged
  (fail-closed, not repaired).
* **Classification:** wrong-response behaviour with a containment contrast;
  upstream defect status and post-cancellation contract remain untriaged.

### 2.4 sqlx 0.8.6 direct `PgConnection`

* **Normal path, covered:** preflight and two pre + two post calibrations.
* **Contract:** no public direct-handle recovery guarantee; the pool release
  path explicitly warns that a dropped `Executor` may leave the connection
  inconsistent and pings/hard-closes.
* **Reached intervention:** two realised boundaries (prepare round-trip and
  bind/execute round-trip); 48 cuts, 12 baselines.
* **Mechanism:** `wait_until_ready()` before every send
  (`executor.rs:207-208`, `:237-238`), `pending_ready_for_query_count`
  incremented per Sync (`:157-166`, `:276`, `:282-283`), drain through
  `ReadyForQuery` (`connection/mod.rs:85-99`, `:111-119`).
* **Contrast:** 0 violations, all next operations returned their own values;
  M1 9 Pre + 3 Post; `residue=32` in the Post trials then 0 after the survivor.
* **Classification:** mature defense / counterexample; not a new defect.  A
  mid-partial-write cut remains an untested source hazard.

### 2.5 Pool layer

* **psycopg_pool:** discard on the guard error; caller sees `CancelledError`
  (propagate) or `ProgrammingError` (swallow); retry-inside-checkout silent;
  next checkout clean (3/3).
* **asyncpg pool:** reset; same pid, clean next checkout (3/3); swallow gives
  no error; retry-inside-checkout silent.
* **Contract boundary:** a pool can only enforce reuse qualification at
  release, not truth inside a checked-out connection.

## 3. Excluded, unresolved, and not counted

* **Linux multicast and Skytable:** another task; read-only reference, no
  edits, no reruns.  Their patch files and logs are outside this commit.
* **psycopg double cancellation:** known open #1444 / PR #1448, not a new
  root cause; reported separately because it is a distinct caller model.
* **asyncpg exit-commit race:** 3/3 immediate failures vs 1/1 success after
  150 ms is a direction, not a bounded distribution; not called a stable
  failure.
* **psycopg sync `KeyboardInterrupt` during `__enter__`:** the same generator
  path is likely affected, but it was not run; not claimed.
* **mysql-connector cut 4:** the COMMIT may or may not reach the server; the
  outcome is a real race and is not counted as a stable false success.
* **mysql-connector coverage:** no MariaDB, TLS, compression, pool, prepared
  statements, multi-row, multi-handle, or direct `cmd_reset_connection()` /
  `ping(reconnect=True)`.
* **sqlx:** no error-response target, no partial-write cut, no 0.9.0 rerun,
  no pool ladder; the source-level partial-write hazard is not observed.
* **pool acquire:** max_size=1, one waiter, callback-created wake-up
  suspensions, single cancellation; asyncpg handoff-before-resume is not
  reachable through the public API.
* **Earlier round-3 gaps not reopened:** tiberius/H1Client; the old
  aiomysql/psycopg/asyncpg B2/B3 zeros remain invalid as defect evidence (they
  are retry-error checks, not behaviour checks).
* **Not used for rates:** repeated positives across runs, different
  client families, or B1/B2/B3/M1 counts from different executions are not
  paired ablations; no detection-rate claim is made.

## 4. The commonality hypothesis: support, counterexamples, rewrite

**Supported.**  psycopg and asyncpg both have a real entry-phase inconsistency:
local transaction bookkeeping is mutated before the `async with` block owns the
cleanup, cancellation interrupts the handoff, and a natural retry silently
nests in the abandoned transaction.  The actual state (INTRANS server, open
outer transaction) is not consistent with the retry's success report or with
reuse: the connection is neither clean nor marked bad.

**Counterexamples / limits.**
* Body-phase cuts are clean in both drivers: rollback runs, retry visible.
* sqlx shows the same unfinished-exchange shape ending in a drain, not a stale
  association; the protocol state machine is the difference.
* Pool release discards or resets, restoring reuse qualification across
  checkouts, even though the driver trap remains.
* mysql-connector's root cause is the same *family* (unread reply + false
  success) but not the same mechanism as transaction-entry bookkeeping; its
  recovery calls are the false-success path, not a retry savepoint.
* Exit-commit cancellation is inherently ambiguous; a client cannot promise a
  known outcome, and asyncpg fails loudly rather than silently there.

**Rewrite (search lead, not a finding).**  The high-value commonality is:

> cancellation after a local state mutation that the matching async cleanup
> will not undo, followed by a successor operation that is allowed to proceed
> as if the abandoned state were complete.

This predicts where to look next: transaction/pool/lease/session entry and exit
bookkeeping, any client whose "retry" can become a nested operation, and any
cleanup path protected by a guard that itself depends on the mutated state.
It does **not** predict body-phase cancellation or commit ambiguity.

## 5. Ready for the patch line / next round

**Ready now (with the stated limits).**
* psycopg: a minimal entry repair is feasible: catch `BaseException` from
  `AsyncTransaction.__aenter__` after `status == ACTIVE`, roll back through
  `_rollback_gen`, then re-raise; or fail closed by closing the connection.
  The monkeypatch variant passes the probe 2/2 and M1 0/12 and is a concrete
  upstream-style patch candidate.  It needs a normal-path and nested-savepoint
  regression test, and the sync `__enter__` path should be checked.
* asyncpg: do not patch `__aenter__` to roll back synchronously (racy); the
  caller-side fix is `reset()` or reconnect, and a fail-closed terminate is a
  containment.  An upstream fix would have to settle the in-flight
  cancellation before rolling back, or clear `_top_xact` only after the
  server state is known.
* mysql-connector: the patched copy is a containment, not a repair.  It
  preserves `CancelledError` (B1 0/12) and makes reuse loud; the server-side cleanup after transport abort is asynchronous and is not a
  client-visible rollback acknowledgement.  A real fix needs to
  drain or poison at cancel time and to define what `rollback()` promises on a
  poisoned stream.
* pool layer: psycopg_pool's discard and asyncpg's reset are already safe;
  recommend release before retry as a tested workaround for this entry
  scenario; do not invent a documented caller contract from that workaround.  The stale psycopg_pool waiter is a
  small separate accounting fix.
* sqlx: no patch; keep the 48-cut drain test as a defense regression for
  future versions.
* Pool acquire: no patch for connection accounting; the stale `_waiting` entry
  is the only concrete owner-level defect, with `max_waiting` as the
  user-visible trigger.

**Next round (ordered by information gain).**
1. psycopg sync `KeyboardInterrupt` entry and sync pool return; does the same
   `_num_transactions` dead end occur, and is `close()` the only exit?
2. asyncpg exit-commit distribution: bound the stale-protocol window, and test
   `reset()` immediately after the failed retry; separate "commit applied,
   client failed" from "commit not applied, client failed".
3. Retry-inside-checkout in a realistic framework (psycopg_pool /
   SQLAlchemy / anyio cancel scope): does the pool release happen before the
   retry, and does the caller observe the unexpectedly uncommitted retry write?
4. mysql-connector cut-4 commit race, prepared statements and multi-row
   results; check whether a reader can get a stale *result set* rather than a
   stale scalar.
5. A cross-driver entry-cancellation scenario in the exploration harness
   (not the core engine): same shape, independent observer, recovery modes.
6. Only after those: consider an upstream-quality patch for psycopg and a
   test that fails on the unpatched driver.

## 6. Integration review (2026-10-22)

The original round was committed separately as `442a34b` on `r4-mainline`.
The main checkout integration preserves its recorded logs and incorporates the
review corrections below; Linux/Skytable patch work is not part of this change.

- The pool phase probe now uses the real async context-manager protocol for
  both drivers, records `enter` before entering it, and rejects a trial that
  did not actually cancel during entry. Previously its asyncpg branch called
  `__aenter__` without a matching exit if execution reached the body, and the
  phase field stayed `setup`. The original entry-cut records are retained,
  not silently rewritten.
- Three no-service tests cover successful/body-cancel cleanup, failed-entry
  behaviour for both drivers, and propagation of an unrealized-entry trial
  failure. All 17 Python adapter/harness tests passed.
- A fresh isolated PostgreSQL rerun covered four pool caller behaviours for
  each driver (8 trials). All six non-baseline cuts reached entry; the next
  checkout's write was visible in all eight trials. Retry within the original
  checkout still returned success with no independently visible row. psycopg
  discarded the affected connection; asyncpg reset and reused it.
- Four additional direct-entry probes reproduced the original uncommitted
  retry for both drivers, and visible retry after the psycopg experimental
  entry repair or asyncpg reset respectively. These are targeted reruns, not
  a rerun of every original matrix or an estimate of stability.
- The sqlx crate passed `cargo check --locked --offline --all-targets` and
  `cargo clippy --locked --offline --all-targets -- -D warnings`; review
  removed an unnecessary password-buffer allocation in the companion probe.
  Its original 48-cut live matrix and the MySQL recovery matrix were inspected
  as prior results, not independently rerun during integration. Upstream issue
  status/newness in this report remains the original author's search result.

The mysql-connector experiment is stored as a unified diff plus notes, not the
third-party package copy. Its cancellation-count guard is not a reliable
"currently unwinding" predicate; the catch-and-reuse-in-the-same-task path is
still untested. Accordingly its recorded fail-closed behaviour is scoped to
the tested calls, not a complete production fix. Patch publication is deferred.
