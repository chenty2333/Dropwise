# Single-interrupt transaction boundaries in synchronous psycopg 3.3.6

Offline-discovery result, 2026-10-22.  Target: `psycopg` 3.3.6 (sync
`Connection.transaction()`), CPython 3.13.13, PostgreSQL 18-alpine.  All
experiments run against a disposable database in a local container; the only
writes are rows in per-trial tables.  No upstream contact was made.

The short result: **one** asynchronous exception delivered while a synchronous
transaction context is entering or exiting can leave a state that is not a
top-level transaction, is invisible to the public `info.transaction_status`
check in the entry case, and is reused by `psycopg_pool` across checkouts.  The
two silent-loss states are repairable; the one unread-reply state is not safely
repairable and must be failed closed.  A candidate repair is exercised against
the same injections and against the pool.

## 1. Evidence types

The observations below mix four kinds of evidence; they are not equivalent.

| kind | what it is | where |
|---|---|---|
| real signal | `SIGALRM` whose Python handler raises `KeyboardInterrupt`; optional real `SIGINT` from a helper thread | timing sweep, phase matrix |
| deterministic injection | `sys.settrace` raises `KeyboardInterrupt` at one pinned source line | `entry_pre_send`, `entry_after_send`, `exit_pre_commit` |
| source reading | psycopg code paths that explain a state | line references in the text |
| candidate repair | a runtime monkeypatch, not an upstream patch | `fix.py`, fix matrix |

The deterministic injection is explicitly **fault injection**: it constructs a
state at a program point.  The timing sweep separately shows that real signals
reach the same state classes, so the states are not artifacts of the injection
method.  Exact microsecond windows are properties of this host, Python build and
proxy latency; the state classes and their consequences are the result.

Each trial gets a fresh tested connection, a fresh table, an independent
observer connection that never goes through the proxy, and a loopback proxy
that logs PostgreSQL frames.  The observer reads committed rows; it does not
read the tested connection's session.

## 2. The states

`_num_transactions` is private, but it is the counter that public
`commit()`/`rollback()` guards and the transaction-stack check read.  It is
recorded because it is the client-side fact that makes these states durable.

| name | client `info.transaction_status` | `_num_transactions` | libpq `pgconn.status` | server `pg_stat_activity` | how reached |
|---|---|---|---|---|---|
| clean | IDLE | 0 | OK | idle | signal before the counter is published, or after a complete exit |
| **D** `pre_send_local_ahead` | IDLE | **1** | OK | **idle** | after `_push_savepoint` (counter 1) but before `BEGIN` is sent |
| **C** `unread_reply` | ACTIVE | 1 | OK | idle in transaction | after `send_query` but before the reply is consumed |
| **B** `wait_intrans` | INTRANS | 1 | OK | idle in transaction | while `Connection.wait()` blocks after `BEGIN` was sent |
| **S4** `intrans_no_local_owner` | INTRANS | **0** | OK | idle in transaction | during `__exit__`, after the stack was popped, before `COMMIT` was sent |
| exit wait | IDLE | 0 | OK | idle (COMMIT applied) | while waiting for the COMMIT reply |

Pinned program points (verified by `pgsync.case points`):

```
transaction.py _enter_gen:106        yield from self._conn._exec_command(command)
_connection_base.py _exec_command:478  results = yield from generators.execute(...)
transaction.py _commit_gen:140       yield from self._conn._exec_command(command)
connection.py wait:495               return waiting.wait(
```

`entry_pre_send` is produced by injection at `_enter_gen:106`; `entry_after_send`
at `_exec_command:478`; `exit_pre_commit` at `_commit_gen:140`.  `entry_wait` and
`exit_wait` use a real timer and a 300 ms per-direction proxy delay.

The important split is local vs server:

* **D** has an advanced local counter and a server that never saw `BEGIN`.
  `info.transaction_status` reports IDLE, so a state check based on the public
  property sees nothing.  The server has nothing to roll back.
* **B** has both sides open.  The top-level transaction is real on the server,
  and the local counter says a transaction context is still owned.
* **S4** has the server open and the local counter back to zero.  The local
  stack looks clean; there is no local owner of the server transaction.
* **C** has libpq in the middle of an exchange; no Python generator owns the
  pending result any more.

## 3. Real-signal reachability

Sweep: 12 offsets, 5 repetitions, survivor mode, proxy delay 10 ms, real
`SIGALRM`.  60 trials:

| offset (µs) | n | state classes | survivor write visible |
|---|---|---|---|
| 2, 5, 10 | 15 | clean 15 | 15/15 |
| 15 | 5 | clean 3, D 2 | 3/5 |
| 20 | 5 | clean 2, D 3 | 2/5 |
| 25 | 5 | D 4, C 1 | 0/5 |
| 30 | 5 | clean 1, C 3, D 1 | 1/5 |
| 40 | 5 | C 3, D 2 | 0/5 |
| 60 | 5 | C 2, B 3 | 0/5 |
| 100, 200, 500 | 15 | B 15 | 0/15 |

Totals: clean 21, D 12, C 9, B 18.  **Every non-clean state lost the later
survivor write; every clean state kept it.**  The windows are narrow and move
between runs (an earlier sweep put C at 25–60 µs and B from 100 µs); the classes
are stable.

A real `SIGINT` sent from a helper thread during the entry wait produced B in
3/3 trials, with the survivor returning `ok` and not visible — the same
consequence as `SIGALRM`.  The signal number is therefore not the variable; the
timing relative to the Python program points is.

The sweep also shows why the async result from round 4 is not the whole story.
In `AsyncTransaction.__aenter__` the only suspension is
`await self._conn.wait(self._enter_gen())`.  The segment from `_push_savepoint`
to `send_query` is synchronous inside the generator, so a `Task.cancel()` (which
is delivered at a suspension) cannot land between them, and cannot land after
`send_query` with the reply unread.  Those two states are reachable only by a
Python-level asynchronous exception inside the generator, which is what the
sync signal path does.  D and C are therefore sync-only in this code shape.
This is a source-level argument plus the round-4 phase map, not a new async
experiment.

## 4. Consequences

### 4.1 Silent loss (B and S4)

A retry or a later survivor through the official `with conn.transaction()`
returns `ok`, but the observer sees no row:

```
entry_wait        retry ok, survivor ok, rows []     3/3 silent-loss
exit_pre_commit   retry ok, survivor ok, rows []     3/3 silent-loss
```

Mechanism (source): the server is INTRANS, so the new transaction's
`_outer_transaction` is false and psycopg emits `SAVEPOINT "_pg3_2"` /
`INSERT` / `RELEASE "_pg3_2"`.  `RELEASE` is not `COMMIT`; the outer server
transaction stays open.  Closing the connection rolls it back.  The wire
transcript of the retry is shaped exactly like a legitimate nested transaction
(§6).

Recovery is not the same in the two states:

| state | `conn.rollback()` | raw SQL `ROLLBACK` | `conn.transaction()` after raw rollback | plain autocommit insert |
|---|---|---|---|---|
| B (entry wait) | ProgrammingError: explicit rollback forbidden | ok (server IDLE) | **AssertionError** (counter still 1) | ok, visible |
| D (pre-send) | ProgrammingError | ok (server already IDLE) | **AssertionError** (counter still 1) | ok, visible |
| S4 (exit pre-commit) | **ok** (counter 0) | ok | **ok** | ok, visible |
| C (unread reply) | error | error | error | error |

The raw SQL rollback is a server-only repair: it leaves the local counter at 1,
so the next `with conn.transaction()` still fails with the bare `AssertionError`.
The only complete repair in-process is a reconnect (or a non-public counter
reset).  This is a concrete example of recovery permission being decided by
local bookkeeping rather than by the server state: B and S4 have the same
server `INTRANS` state, but `rollback()` is forbidden only in B.

### 4.2 Bare assertion, invisible to the public status (D)

`_push_savepoint` contains `assert not self._conn._num_transactions`.  In D the
counter is 1, so the next `with conn.transaction()` raises
`AssertionError: ` with no message.  The connection is not closed, its
`info.transaction_status` is IDLE, and a plain autocommit write still succeeds.
A pool or middleware that checks `transaction_status` before reuse will classify
this handle as clean.

### 4.3 Unusable but loud (C)

The next operation raises
`OperationalError: sending query failed: another command is already in progress`.
`rollback()`, raw `ROLLBACK`, `transaction()` and a plain insert all fail; only
closing recovers.  There is no silent loss, but the handle is unavailable.  A
candidate repair should fail closed here, not pretend to repair.

### 4.4 Exit wait is ambiguous, not a bug

If the signal arrives while waiting for the COMMIT reply, the server has applied
the commit; the client is IDLE/0, the target row is visible, and a retry writes
a second row.  The caller sees an exception even though the operation completed.
Under an atomic contract (pre or post) this is allowed; under a "must have
completed" contract it is a real violation.  It is listed separately and is not
counted as a defect.

### 4.5 The sync pool reuses the D handle across checkouts

`psycopg_pool` 3.3.3, one connection, default callbacks.

| build | case | same backend reused | next `transaction()` | victim row visible |
|---|---|---|---|---|
| original | entry_wait (B) | 0/3 | ok | yes (new connection) |
| original | **entry_pre_send (D)** | **3/3** | **AssertionError** | **no** |
| fix | entry_wait | 3/3 | ok | yes |
| fix | entry_pre_send | 3/3 | ok | yes |

In B the pool's reset sees INTRANS, tries `rollback()`, gets the guard error and
discards the connection; the next checkout gets a new backend.  In D the reset
sees IDLE and **returns the same backend to the pool**.  The next checkout gets
the poisoned handle: `transaction()` raises the bare AssertionError, an
autocommit insert succeeds, and the pool's normal-exit commit raises
`ProgrammingError: Explicit commit() forbidden within a Transaction context`
(the row is not committed).  This is the cross-checkout victim that round 4 saw
for the async entry trap, but here the pool's own server-state check cannot see
it and the connection is reused.

This is not a claim that every pool usage loses a write: the victim may fail
loudly at the commit, and the pool discards the handle afterwards.  The point is
that the handle is handed to an unrelated next user, the failure is a bare
assertion or a commit error far from the interrupted entry, and the pool's
`transaction_status`-based reset is not an ownership check.

### 4.6 Legitimate nesting and the abandoned transaction look the same

Three executions, one observer, same server state after the inner block:

| execution | inner row visible after inner block | after outer exit | client after inner block |
|---|---|---|---|
| legitimate outer + nested inner | no | **yes** (COMMIT) | INTRANS/1 |
| interrupted entry + retry | no | **no** | INTRANS/1 |
| legitimate outer, dropped before commit | no | **no** | INTRANS/1 |

Wire SQL (from `contrast-frames.jsonl`):

```
legitimate:   BEGIN; SAVEPOINT "_pg3_2"; INSERT; RELEASE "_pg3_2"; COMMIT
abandoned:    BEGIN; [CancelRequest]; SAVEPOINT "_pg3_2"; INSERT; RELEASE "_pg3_2"
dropped:      BEGIN; INSERT
```

The retry inside the abandoned transaction has the same savepoint shape as the
legitimate nested transaction.  What differs is not a wire fact but whether a
live caller still owns the outer transaction and intends to commit it.  The
server reports INTRANS in all three; a kernel or protocol observer that sees
`SAVEPOINT`/`RELEASE` cannot tell legitimate nesting from an orphaned outer
transaction.  A guard needs an ownership/liveness witness, not another packet
inspection.  This is the strongest argument in this round against treating
server session state alone as an obligation ledger.

## 5. Candidate repair and fix contrast

`fix.py` monkeypatches the *sync* `Transaction.__enter__`/`__exit__`.  On an
interruption it:

1. leaves a clean state untouched;
2. if `_num_transactions > 0`, `pgconn.status` is OK and the server is not
   ACTIVE, runs the library's own `_rollback_gen` (pops the stack, sends
   `ROLLBACK`; for a nested entry it would send `ROLLBACK TO`/`RELEASE`, but a
   nested entry with an uncertain savepoint is failed closed);
3. if libpq is ACTIVE (unread reply), closes the connection;
4. on an interrupted exit with `_num_transactions == 0` and server INTRANS,
   sends a raw `ROLLBACK` (the stack is already popped; reusing `_rollback_gen`
   would pop again);
5. re-raises the original interruption.

Same injections, 3 trials per phase/mode:

| phase | original | fix |
|---|---|---|
| entry_wait | silent-loss 3/3 | **post 3/3** |
| entry_pre_send | explicit-failure 3/3 | **post 3/3** |
| entry_after_send | explicit-failure 3/3 | explicit-failure 3/3 (closed; fail-closed) |
| exit_pre_commit | silent-loss 3/3 | **post 3/3** |
| exit_wait | post 3/3 | post 3/3 |
| body | post 3/3 | post 3/3 |

Pool with the fix: B and D both reuse the same backend and the next transaction
and victim write succeed (3/3 each).  The fix repairs the two silent states and
keeps the pool available; the unread-reply state is deliberately converted to a
loud closed connection.

What the fix does not do: it is not upstream; it is a monkeypatch, tested only
against these injections and this version; nested entry is closed rather than
repaired (it cannot tell whether `SAVEPOINT` was sent); an interrupted nested
exit is not repaired, so the caller's outer transaction may retain an unreleased
savepoint; an interrupted top-level exit is resolved to `Pre` (rolled back), so
a caller that required `Completed` will see a violation; async psycopg is
untouched; the bare assertion is not given a message.  An upstream patch needs tests in
psycopg's suite, a decision on the public contract (the maintainers have declined
to document one, see §7), and probably a small public/private state witness
rather than a counter peek.

## 6. Relation to existing knowledge

* psycopg #1293 (closed): signal-driven exceptions can land at arbitrary points
  around commit; the maintainer declined to document the expected behaviour,
  calling it emerging behaviour.  This round does not contradict that; it
  measures what the emerging behaviour is on the sync path.
* #1384 (closed): `SystemExit` is in `_INTERRUPTED`; our repair treats it like
  `KeyboardInterrupt`.
* #1444 (open, 2026-10-16) / #1448 (open): a **second** Ctrl-C during
  `Connection.wait()` cancel cleanup can leave the connection ACTIVE/busy; #1448
  proposes closing.  Our C state is a **single** interruption with an unread
  reply; B is a single interruption whose cleanup completes.  A close-on-
  interrupt-during-cleanup rule would remove B/S4 at the cost of the connection;
  our repair keeps them usable.  Neither issue covers D or the pool reuse.
* Round 4 (async psycopg): `AsyncTransaction.__aenter__` publishes the local
  counter before `BEGIN`, so a cancellation at the entry wait makes a retry
  nest as a savepoint and lose the write silently.  This round confirms the same
  trap on the sync path (B) and adds the sync-only D/C/S4 states, the recovery
  asymmetry, the pool victim, and a fix contrast.
* The SQLx drain path remains the mature comparison: it treats "reply not
  consumed" as a connection-fatal condition rather than trying to resume.  Our
  C state is the psycopg analogue; B/S4 are the cases where a repair is possible
  because no reply is outstanding.

## 7. Limits and threats to validity

* 60 real-signal trials and 3 trials/cell in the phase matrix; these are
  existence and consequence measurements, not production probabilities.
* One host, Python 3.13.13, psycopg 3.3.6, PostgreSQL 18-alpine, pool size 1.
  Timing windows will differ on other hosts; the state classes are tied to this
  source shape.
* `SIGALRM` handler vs CPython's `SIGINT` handler: both raise an asynchronous
  exception from a signal frame; a real `SIGINT` check reproduced B.  Signal
  coalescing and handler reentrancy are not exhaustively tested.
* The deterministic line injection is synthetic.  Real reachability of D/C is
  shown by the sweep, but the exact landing line for a real signal is not
  pinned; the generator frame can be suspended when the handler captures the
  stack.
* Observer reads committed rows only; a row invisible in the observer could in
  principle be visible in another session at a different isolation level.  The
  tested connections use the default isolation; the observer uses the default.
* `autocommit=True` on the tested connection makes the plain-insert probe
  succeed in D.  A non-autocommit caller would see the implicit transaction's
  commit fail; this is noted, not measured as a separate matrix.
* The fix is a research prototype.  Nested entry (closed, not repaired), nested
  exit (not repaired), pipelines, `force_rollback`, two-phase transactions, and
  async are not covered by the fix matrix.
* No kernel-level injection was used.  An eBPF observer could see `BEGIN`,
  `ROLLBACK`, `CancelRequest` and `INTRANS`, but not ownership; §4.6 shows why
  that is insufficient for the legitimate-vs-abandoned distinction.

## 8. Most valuable next steps

1. Turn `fix.py` into an upstream-quality patch with tests for entry/exit,
   pool reuse, nested entry, `force_rollback`, and normal paths; decide whether
   the repair belongs in psycopg or in the pool's reset callback.
2. Add an explicit ownership/liveness witness (a live transaction-stack owner)
   to the driver or pool, and test it against the legitimate nested transaction
   in §4.6 to measure false positives.
3. Port the field contract (see `METHOD-fields.md`) to the Rust engine and
   auto-derive effect fields from resource identity; the sync harness already
   uses the Python judge unchanged.
4. Test the same states on a target not used in the design (SQLAlchemy sync,
   anyio, psycopg pipeline) and look for false positives/negatives.
