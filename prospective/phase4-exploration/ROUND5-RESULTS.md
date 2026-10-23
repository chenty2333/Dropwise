# Round 5 results: handle comparison, cut scale, delivery handoff, and four appended groups

## Current corrections (reviewed 2026-10-23)

The stage-comparison harness has been repaired and rerun on CPython 3.15.1.
The omitted partial-send regression is now included, and recovery uses two real
cancellation requests. The comparison still does **not** establish general defect
coverage or the earlier 83% speedup; the corrected sample contains a missed state.
Historical measurements below remain tied to their original environments.

### 1. Actual patch vs experimental strategy (asyncpg, psycopg, MySQL)

The asyncpg `--fixed` arm was the old terminate-on-entry monkeypatch, not the
submitted `_top_xact` registration repair. Re-run with the loaded package path
and source checked (`/home/ava/Worktrees/asyncpg/repo`, HEAD `ff1bb8d`; every
record carries `package.file`, version and `start_has_top_xact_clear`):

- **Actual asyncpg patch:** same-lease ordinary `INSERT` returns `INSERT 0 1`
  but the independent observer sees 0 rows; the write is rolled back on pool
  return. `reset()` then INSERT is visible. The transaction API now refuses
  explicitly (`cannot use Connection.transaction() in a manually started
  transaction`). A legitimate outer transaction survives an ordinary nested
  failure and a cancelled nested start, and commits visibly.
- **Experimental terminate arm:** same-lease reuse is refused, but a cancelled
  *nested* start terminates the whole connection and kills the legitimate outer
  transaction. That is a strategy result, not a property of the submitted patch.
- **Actual psycopg patch:** `AsyncTransaction._recover_entry_failure()` present;
  entry cuts are clean in the probe and the complete differential has zero
  violations (baseline: four `Neither` violations at psycopg entry k=1,2).
- **Actual MySQL patch:** `e7a02b6` is selected by the loaded worktree, not
  `fix/patched`. Entry, nested and commit-in-flight cancellation poison the
  socket; rollback/reset/successor all raise `OperationalError: MySQL Connection
  not available`; the observer sees no uncommitted row; the normal control is
  visible. The old experimental build raises `InterfaceError: Cancelled during
  execution; connection is not reusable` instead.

### 2. Fair method comparison

`effect_judge.py` is a target-effect-only ablation. The original calibrated
method (`explore_observed`, survivor + pre/post calibration + Atomic) was run on
the same target and cutpoints, with the same two reps and window. It **also
finds the psycopg entry defect** (4/4 `Neither` at k=1,2); the effect-only
ablation misses it (8 `no_effect` over two reps) and labels the legitimate
commit-after-cancel exit cuts `visible_after_cancel`, which is not an Atomic
violation. The actual psycopg patch removes the complete-differential violation.
The new probe's discovery increment over the complete differential on this
target is therefore **zero**; its increment is classification (invisible write
vs explicit transaction-API refusal vs pending outer vs blocked session) and
fresh-reference calibration. The complete differential costs ~25 s psycopg /
~16 s asyncpg for 2 reps plus calibration; the probe costs ~21-26 s /
~13-16 s for the same cuts and reps. Each method's code/reuse and remaining
limits are in `python/pg-mysql/method_compare/RESULTS.md`.

### 3. Separated state, black-box effect and fresh reference

The probe now has `--state-only` and effect mode. In effect mode the transaction
state is recorded but does not short-circuit the write. Results:

- psycopg baseline entry: state `open_transaction`, effect `invisible_write`,
  reference clean.
- psycopg actual patch entry: state clean, effect clean.
- asyncpg baseline entry: state `open_transaction`, effect `invisible_write`.
- asyncpg actual patch entry: state still `open_transaction`, effect
  `transaction_api_error` (explicit refusal). State-only cannot distinguish the
  actual patch from baseline here; the write does.
- legitimate outer: `effect_pending_outer`, then visible after commit (2/2);
  nested rollback leaves the marker absent (2/2).
- session clean (2/2); read-only `blocked_by_session` (2/2), not an anomaly.
- pool return/reborrow is clean for baseline and actual; same-lease ordinary
  reuse is not.

The asyncpg actual patch does **not** protect a same-lease ordinary operation
or a commit-in-flight cancellation; it protects the local registration boundary
(explicit transaction-API refusal, outer identity guard) and leaves recovery to
an explicit reset/rollback/close. The psycopg actual patch recovers entry but
does not cover the partial-send path.

### 4. Formal psycopg partial-send cancellation repair (finalized)

Implemented on an independent local branch `codex/partial-send-cancel`
(`/home/ava/Worktrees/psycopg/partial-send`, based on `origin/master` `99356800`),
not on the transaction-entry PR branch and not pushed.

The initial commit `fbcbf651` used `PQflush() != 0` to detect buffered output.
That check had an interleaving window: `PQflush()` actively pushes data to the
socket; if backpressure clears while cancellation unwinds, `PQflush()` flushes
and returns 0, but the generator remains suspended at write readiness
(`WAIT_RW`), causing `TypeError: an integer is required` on C generators and
executing the cancelled query on pure Python. The claim that `PQflush() == 0`
proves the send generator completed is **retracted**.

The updated fix wraps the generator in `_WaitGenTracker(gen)` within
`AsyncConnection.wait()`. If `last_wait is None` or `last_wait & WAIT_W`, the
request was cancelled during the send stage (or prior to read readiness); the
connection is closed immediately (`self.pgconn.finish()`) and `_INTERRUPTED` is
re-raised without touching `PQflush()` or matching exception strings. Fully sent
requests (`last_wait == WAIT_R`) keep the existing cancel-and-drain path. A
second interruption during recovery also fail-closes.

Refinements completed in this final round:
1. **Production loading logic removed:** `pq/misc.py` changes (`PSYCOPG_LIBPQ` and
   `psycopg_binary.libs` search) were completely reverted; libpq discovery for pure-Python
   testing is handled strictly within `tests/conftest.py`.
2. **Comment and contract boundaries refined:** Overreaching claims ("never executed
   remotely", "impossible to recover") were removed from `connection_async.py` comments,
   retaining concrete stage detection, fail-closed policy, and lease loss.
3. **Disentangled interleaving regressions:** Split into two explicit scenarios in
   `tests/test_concurrency_async.py`:
   - Scenario A (`test_cancel_interleaved_proxy_release_only_closes`): only resumes proxy
     reading without active test flush; verifies cancellation and fail-closed state.
   - Scenario B (`test_cancel_interleaved_active_flush_closes_safely`): actively drains
     buffer, explicitly asserts the prerequisite `PQflush() == 0` was met, and declares
     both 0 or 1 rows as valid wire effects while guaranteeing connection safety and
     cancellation propagation.
   Baseline fails Scenario B with `TypeError` on C binary and commits uncancelled on Python;
   all seven targeted tests now pass on both backends under Python 3.15.1.
   The previously omitted `test_cancel_while_sending_closes_connection` is included.
4. **Independent server-side cleanup & robust negative controls:** `min_repro.py` never
   calls `conn.close()` to rescue driver omissions. It verifies driver self-close, records
   independent close timings, enforces bounded waits (5s) for `pg_stat_activity` and
   `pg_locks` cleanup, and acquires `ACCESS EXCLUSIVE NOWAIT` table locks. Automated negative
   controls (`driver-unclosed`, `wrong-exception`, `unconfirmed-cleanup`) all fail cleanly
   with exit code 1 without being masked by `finally` blocks.

### 5. Stage-informed comparison: corrected on Python 3.15

The previous comparison was invalid: runner teardown was misclassified as driver
closure, coarse verdict sets hid distinct states, repeated nested operations were
collapsed, second cancellation was not demonstrated, and workloads differed.
Those results and the claimed 83% speedup are withdrawn. The existing
`stage_experiment/*_results.json` files now contain the corrected run, not that
old output.

The current harness:

- establishes connections before either replay method starts; setup/teardown
  suspensions are excluded from both methods;
- observes connection state, independent committed row identities, backend
  presence/transaction state and relation locks before successor probes and
  harness close; closed backends get a bounded disappearance check;
- compares operation identity + actual stage + outcome/exception + state +
  successor result + effects + cleanup + cancellation delivery, not verdict labels;
- separates outer/inner operations and preserves unclassified stages rather than
  inventing send/read signals; Method B selects the first suspension per
  `(operation, stage)` pair, while Method A enumerates all target suspensions;
- stops replay after failed/empty calibration and reports failed setup, missing
  targets or undelivered second cancellations as Unknown, never zero omissions;
- uses the same backpressure hold and 100 ms timed release for calibration and
  both methods; release is not conditional on the chosen cut;
- instruments real `_try_cancel()` entry and verifies two actual cancellation
  requests and delivery. No synthetic exception or extra recovery sleep is used.

CPython 3.15 separates the Python Task registry from the native task APIs. The
shared `engine.PureTask` now registers lifecycle and current execution with both,
so `current_task()`, `all_tasks()`, cancellation counts and `asyncio.timeout()`
work inside injected tasks. Stage and yield experiments use the same base.
asyncpg's plaintext proxy connection explicitly disables TLS; its previously
empty fragmentation calibration now succeeds. This does not establish a general
proxy capability beyond the exercised plaintext PostgreSQL flows.

Environment: CPython **3.15.1**, PostgreSQL **18.6**, psycopg **3.3.7.dev1** loaded
from `/home/ava/Worktrees/psycopg/partial-send/psycopg`, binary/libpq
**3.3.6 / 18.6**, asyncpg **0.32.0** (released package, not the separate entry-fix
worktree). The partial-send branch does not include the transaction-entry repair.

| Driver / scenario | Yield trials A | Stage trials B | A observations missing from B |
|---|---:|---:|---:|
| psycopg short transaction | 3 | 3 | 0 |
| psycopg nested transaction | 6 | 6 | 0 |
| psycopg fragmented response | 8 | 3 | **1** |
| psycopg partial-send backpressure | 10 | 4 | 0 |
| psycopg query / recovery double cancellation | 20 | 1 | 0 |
| asyncpg short transaction | 3 | 3 | 0 |
| asyncpg nested transaction | 6 | 6 | 0 |
| asyncpg fragmented response | 3 | 3 | 0 |

All eight calibrations and all **88** cancellation trials were realized in this
run (59 A + 29 B); "valid" means usable observations, **not** absence of failures.
In the fragmented psycopg case, A observes cancellation during `outer.enter`
leaving an open transaction (`INTRANS`, backend `idle in transaction`), with a
successful `SELECT 42` but no clean transaction handoff. B's first sample of that
same stage misses this state. Thus successful successor queries alone are also
insufficient, and the existing entry failure is not a new partial-send defect.
The **21/21** double-cancel trials record both the second request and actual
receipt, closed-handle rejection and observed backend disappearance.

This is one matched run, not a repeated statistical or seeded-defect campaign.
Earlier development reruns varied in the precise missing-state set, which is
another reason not to claim stage equivalence. Trial counts are descriptive;
there is **no accepted coverage-preserving speedup** and no general defect-coverage
claim. The existing open-transaction outcomes in the other cases remain failures,
not proof that the partial-send patch repairs every cancellation path.

Validation on the new environment:

- **86** repository tests (engine, classifier, harness negative controls and
  PostgreSQL min-repro controls) pass.
- psycopg's **7/7** focused regressions pass on binary and pure Python. The
  recovery test independently confirms `PgSleep`, two `Task.cancel()` calls,
  delivery in recovery, `cancelling() == 2` and fail-closed rejection.
- The five-file concurrency/cursor/pipeline/waiting/transaction sweep passes
  **275 tests, 5 skipped per backend**. This is not an unqualified full-suite run.
- The follow-up source-generation repair uses upstream `# ASYNC` guards for
  async-only tracking and recovery. `async_to_sync.py --check --all` now passes
  on Python 3.15.1, with generated `connection.py` unchanged. The generator's
  canonical-runtime warning (3.14) remains informational; no lint rule was
  weakened. Both 275-pass/5-skip async sweeps were rerun after this repair.
  The binary-backend sync concurrency/waiting/transaction control also passes:
  **347 passed, 26 skipped**. Black, flake8 and targeted mypy pass.

The local environment setup and interpreter selection are documented in
[`research/python-retro/README.md`](../../research/python-retro/README.md).
The research and psycopg changes remain local. The separately authorized
diesel_async #305 review follow-up was committed, pushed and replied to; it does
not publish these research or psycopg changes.

Bottom line: the actual patches are narrower than the experimental arms; the
complete differential already detects the psycopg entry defect, so the probe's
contribution is contract classification, not new discovery; the asyncpg
registration patch still requires caller recovery on a same lease; and the
psycopg partial-send defect now has a stage-scoped fail-closed repair with
discriminative failing-baseline/passing-fixed regressions and independent
backend cleanup verification.

New work only; round-4 results and the user's existing logs are not rewritten.
All experiments use disposable local databases and unique tables.  The main
artifacts are under `python/pg-mysql/`; commands and raw JSONL are linked from
each sub-report, not duplicated here.

## 0. Short answer

- The **actual** asyncpg registration patch does not refuse same-lease
  ordinary reuse: `INSERT 0 1` is returned while the independent observer sees
  nothing, and only reset/rollback/close or a pool return makes the lease safe.
  It does protect the transaction-API boundary by explicit refusal and keeps a
  legitimate outer transaction alive on a nested failure. The older terminate
  arm's refusal is not a property of the submitted patch.
- The complete calibrated differential finds the psycopg entry defect
  (`Neither` at k=1,2; 4/4), and the actual psycopg patch removes it. The
  effect-only ablation misses it. The new probe finds the same psycopg defect,
  so its discovery increment over the complete differential is **zero**; its
  increment is classification (invisible vs refused vs pending vs
  unprobeable) and fresh-reference calibration.
- The measured cut scale is 6 (psycopg direct), 3 (asyncpg direct), 12
  (psycopg outer+inner savepoint), and 4 (warm pool checkout); nested operation
  is the main underestimate of a "transaction boundary". Automatic runtime is
  ~1.8-24 s per target for counts + all cuts; target-specific setup is the main
  authoring cost.
- The pool delivery window is correctly protected in psycopg_pool/asyncpg;
  no leak or capacity loss was found. Pool return/reborrow is clean for baseline
  and actual patches, but a same-checkout post-cancel reuse can still silently
  lose a write, so the delivery window and the lease-use window are separate.
- The partial-send cancellation defect now has a source-level fail-closed
  repair with failing-baseline/passing-fixed tests on an independent local
  branch. It is not submitted and does not cover sync or pipeline partial
  sends.

## 1. Old/new handle comparison (Work 1)

`successor_probe/probe.py` + `run_pg.py` + `run_pool.py`;
details in [`python/pg-mysql/successor_probe/RESULTS.md`](python/pg-mysql/successor_probe/RESULTS.md).

Contract: a fresh nonce and column metadata must echo; in **effect mode** the
old handle gets a unique marker write in a normal transaction and bounded
independent visibility, even if its state says it is already in a transaction.
In **state mode** the probe stops after the state/session diagnosis.  The
reference is a fresh direct handle.  The probe compares handle behaviour, not
the target's intended effect.

Every run records the loaded package path/version and the actual-repair marker.
Repair selection is by interpreter path, not by flag name; `--repair actual`
refuses to run if the marker is absent.

| target | state mode | effect mode | reference | compare |
|---|---|---|---|---|
| psycopg baseline entry k=1,2 | open_transaction | **invisible_write** | clean | **old_anomaly** |
| psycopg actual patch entry | clean | clean | clean | consistent_clean |
| psycopg body/exit | clean | clean | clean | consistent_clean |
| asyncpg baseline entry k=1 | open_transaction | **invisible_write** | clean | **old_anomaly** |
| asyncpg actual patch entry k=1 | open_transaction | **transaction_api_error** | clean | **old_anomaly** |
| asyncpg body/exit | clean | clean | clean | consistent_clean |
| psycopg actual legit outer | open_transaction_allowed | effect_pending_outer | clean | pending; visible after commit 2/2 |
| psycopg actual nested | open_transaction_allowed | effect_pending_outer | clean | absent after rollback 2/2 |
| psycopg actual session | clean | clean | clean | consistent_clean |
| psycopg actual read-only | clean | blocked_by_session | clean | old_not_applicable |

The asyncpg actual-patch row is the key separation: state alone still says
`open_transaction` (as in the baseline), but the write gets an explicit
transaction-API refusal rather than an invisible success.  State-only evidence
cannot distinguish the patch from the baseline here; the black-box write does.

Where the probe needs extra semantics:

- An open transaction is only an anomaly under the declared handoff contract;
  a legitimate outer transaction must be declared and is reported as pending,
  not clean.
- `blocked_by_session` / timeout is **unprobeable**, not clean.
- A committed exit cut looks clean and visible; the probe cannot know whether
  that commit was the intended effect or a legitimate cancellation-after-commit.
- A pool reset/reborrow can repair the handle before the next checkout; this
  does not prove a retry **inside** the same checkout was safe.
- The reference is direct while the tested handle goes through the delay proxy;
  it is a same-server fresh-handle baseline, not a byte-identical path.  It is a
  calibration control, not a source of new defect discovery.

## 2. Cut scale and method increment (Work 2)

`cut_scale.py`, `complete_judge.py` and `effect_judge.py`; details in
[`python/pg-mysql/method_compare/RESULTS.md`](python/pg-mysql/method_compare/RESULTS.md).

| target | K (5 normal runs) | source awaits | realised cuts | overrun K+1 | phases | wall time (counts + cuts) |
|---|---:|---:|---:|---:|---|---:|
| direct psycopg CM | 6, all | 5 | 6/6 | unrealised | enter 2, body 2, exit 2 | 13.23 s |
| direct asyncpg CM | 3, all | 5 | 3/3 | unrealised | enter 1, body 1, exit 1 | 10.65 s |
| psycopg outer + inner savepoint | **12**, all | 4 | 12/12 | unrealised | outer_enter 2, outer_body 2, inner_enter 2, inner_body 4, outer_exit 2 | 24.22 s |
| psycopg pool checkout | 4, all | 2 | 4/4 | unrealised | enter 1, body 1, body_wait 2 | 1.80 s |

K is stable over five repeats but this is not exhaustive scheduling.  K is not
the source-await count: psycopg yields twice per exchange (6), asyncpg once (3),
and the conditional rollback await is not observed on the normal path.  The
pool sample was warm, so no checkout handoff cutpoint appeared; Work 3 covers
the delivery window separately.

Method comparison on the same entry defect (same target, cuts, allowed
outcomes and window; two reps for all three methods):

| method | psycopg baseline | psycopg actual | asyncpg baseline | asyncpg actual | runtime |
|---|---|---|---|---|---|
| complete calibrated differential (`explore_observed`) | **4 `Neither`** (k=1,2) | 0 violations | 4 `Neither` (entry + commit-in-flight) | 2 `Neither` (commit-in-flight) | 24.7-25.2 s / 16.4-16.5 s |
| effect-only ablation | 8 `no_effect`; 4 `visible_after_cancel` | same | 6 `clean_completed`, 6 `no_effect` | 6 `clean_completed`, 6 `no_effect` | 12.8-13.3 s / 12.9 s |
| new probe (effect mode) | 4 `invisible_write` | 12 clean | 2 `invisible_write` (entry) | 2 `transaction_api_error` (entry) | 20.6-26.2 s / 13.0-15.8 s |
| targeted phase test | entry contamination | psycopg recovered | entry contamination | explicit refusal / outer guarded | sub-second per cut |

The complete differential **also finds** the psycopg entry defect; the probe's
discovery increment over it is zero. The effect-only ablation misses the entry
defect and describes legitimate commit-after-cancel as
`visible_after_cancel`, but that label is not an Atomic violation. The probe's
increment is classification and reference calibration. Asyncpg's actual-patch
entry result depends on whether cancellation reaches the server before or after
`BEGIN`; a cut index alone does not pin that wire phase.

Not established: general superiority, message-level cut completeness, or
replacement of semantic phase knowledge.

## 3. Pool delivery window (Work 3)

`pool_handoff/delivery_case.py`; details in
[`python/pg-mysql/pool_handoff/RESULTS.md`](python/pg-mysql/pool_handoff/RESULTS.md).

A legitimate user holds connection A (session GUC + temp table) while B is
assigned to a target caller.  Shapes: normal, early (still queued), delivery
(inside the public check/setup callback), late (after delivery).  Two reps per
shape; psycopg_pool 3.3.3 and asyncpg 0.32.0.

Result: **no defect in this window**.  All cuts realised; after cancellation
the next acquire succeeds, pool size/available returns to 2, waiting=0, and the
legitimate user's pid/GUC/temp state is intact.  Psycopg's delivery shape can
reuse the same B (the cancellation path returns it and reset makes it
reusable); asyncpg's delivery/late shape gets a fresh holder, which is a
reconnect, not a leak.  Runtime: psycopg 0.024-0.033 s/trial; asyncpg
0.187-0.197 s/trial.

The protection is in the mature code: psycopg_pool catches `BaseException`
around check/delivery and puts the assigned connection back; asyncpg's
`_acquire` returns the holder on cancellation, and the public `setup` callback
runs while that return path is still owned.  The covered window is assignment
through delivery; it does **not** cover a same-checkout retry after an
interrupted transaction entry (section 4).

## 4. asyncpg same-lease reuse after the entry fix (group 1)

`postfix_lease/asyncpg_lease.py`; details in
[`python/pg-mysql/postfix_lease/RESULTS.md`](python/pg-mysql/postfix_lease/RESULTS.md).

The server BEGIN is confirmed by an independent observer before cancellation.
Then the cancellation is caught without return/reset, an ordinary autocommit
`INSERT` is issued on the same connection, and the handle is returned to a
one-slot pool for reset and reborrow.  Three reps per pool mode (two for the
outer/nested control), three variants selected by the loaded package and
verified in every record.

| mode | original 0.32.0 | actual `ff1bb8d` | experimental terminate |
|---|---|---|---|
| normal | committed/visible | same | same |
| entry same-lease INSERT | `INSERT 0 1`, observer **0 rows** | **`INSERT 0 1`, observer 0 rows** | explicit refusal; after return 0 |
| entry reset then INSERT | visible/committed | **visible/committed** | reset refused (already released) |
| entry transaction API | returns ok, write invisible | **explicit `InterfaceError: cannot use Connection.transaction() in a manually started transaction`** | transaction API refused |
| body cancel | visible | visible | visible |
| outer + cancelled nested start | outer survives and commits | **outer survives and commits (2/2)** | **connection terminated; outer commit fails (0/2)** |

The actual patch does **not** make same-lease ordinary reuse safe: it still
returns success with an invisible row.  Its protections are the transaction-API
refusal (no silent savepoint) and the outer identity guard (a nested failure
does not clear the outer registration).  The old terminate arm's explicit
refusal and its outer-transaction kill are strategy results, not properties of
the submitted patch.  `_top_xact = None` is not server IDLE: the observer saw
`BEGIN` and `idle in transaction` at the cut.  The pool return/reset boundary
is clean in all variants; the same-checkout boundary is the hazard.  Explicit
`reset()` is the working same-lease recovery with the actual patch.

## 5. Delayed BEGIN/SAVEPOINT and recovery (group 2)

### psycopg

`delay_recovery/`; details in
[`python/pg-mysql/delay_recovery/RESULTS.md`](python/pg-mysql/delay_recovery/RESULTS.md).
One-way 5 s server→client delay; BEGIN confirmed before cancel.

| recovery | cancel→caller | old server | cancel→successor |
|---|---:|---|---:|
| none | 5.00 s | INTRANS, not cleared in 7 s | 5.01 s |
| `rollback()` | 5.00 s | guard error, still INTRANS | 5.02 s |
| `close()` | 5.00 s | cleared 0.0006 s | 5.01 s |
| experimental fail-closed fix | **10.00 s** | cleared 0.0005 s | 10.01 s |
| experimental fix + second cancel | **0.10 s** | cleared 0.0007 s | 0.11 s |
| nested original rollback | 5.00 s | outer exit `OutOfOrderTransactionNesting`, not cleared | — |
| experimental nested fix | 15.01 s | cleared; outer rollback ok | 20.02 s |

Cancellation delivery is bounded by the delayed BEGIN reply, so an explicit
post-catch recovery cannot start earlier.  The fix rows above are the **older
experimental rollback-in-`__aenter__` monkeypatch**, not the submitted
`_recover_entry_failure()` patch.  The actual patch recovers entry (verified by
the probe and complete differential) and also adds a rollback round trip, but
its 5 s-delay timings were not rerun in this correction; the experimental
timing is retained only as a strategy reference.  A second cancel is an escape
(fast, closes), not a normal repair.  In a one-slot pool, close/replace forces a
new connection: successor 20.01 s; the experimental fix retained the connection
and gave an immediate successor at the price of 10.00 s cancellation.

### MySQL

`mysql_delay/`; details in
[`python/pg-mysql/mysql_delay/RESULTS.md`](python/pg-mysql/mysql_delay/RESULTS.md).
MySQL 9.6.0 + mysql-connector-python 26.7.0 aio; 75 records / 49 cells; reply
held at cancel, none delivered before.

- Cancellation return is immediate (**0.02-0.34 ms**), unlike psycopg: the 5 s
  delay does not delay the caller.
- Original `rollback()` / `reset_session()` can return OK from the **stale
  held reply** while the server transaction is already closed; the successor
  then reads the next queued reply (`rowcount=0`, `[]`) while an independent
  observer sees its row.  Long BEGIN: rollback false OK at 5.0 s, connection
  held 15.15 s; reset recovery 25.0 s, connection 35.16 s.  No usable successor.
- The "patched" rows here are `fix/patched`, the **older experimental direct
  close** (`_close_reason`/`InterfaceError`), not the actual `e7a02b6`
  (`_is_poisoned`/`_abort()`/`OperationalError`).  That direct close evicts at
  cancel (0.14-0.37 ms) and eliminates the stale-reply false OK, but recovery
  fails immediately and **c is unrealised**.  A usable successor needs
  reconnect: ~0.35 s at 50 ms delay, **30-35 s at 5 s**, with c ~0.5 s /
  **45-50 s**.  Direct close is not O(1)/zero-wait to a usable successor.
- Actual `e7a02b6` focused re-check (2 reps, entry/nested/commit-in-flight):
  cancellation poisons the socket and reports `OperationalError: MySQL
  Connection not available` for rollback/reset/successor; the observer sees no
  uncommitted row; the normal control commits visibly.  This confirms the
  explicit-refusal contract, not a usable same-lease successor.  See
  `postfix_lease/RESULTS.md` and `postfix_lease/logs/actual-matrix/mysql-*.jsonl`.
- Re-cancel: 24/24 realised cancels reached the recovery action and caller; 11
  patched rollback/reset second cancels were unrealised because recovery had
  already finished (0.16 ms).  Original reset re-cancel leaves b **Unknown**,
  server ACTIVE, connection occupied 10.15 s.
- Nested SAVEPOINT: full rollback/direct close rolls back the **outer**
  transaction; `ROLLBACK TO SAVEPOINT; COMMIT` commits the outer row at
  5.12 s but the client still misreads the successor (c unrealised).

## 6. Half-frame delivery and send backpressure (group 3)

`fragmentation/`; details in
[`python/pg-mysql/fragmentation/RESULTS.md`](python/pg-mysql/fragmentation/RESULTS.md).
psycopg 3.3.6, fresh PostgreSQL 18.4, one finite split per message with a real
150 ms pause; observer direct.

| mode | K (3 normal runs) | writes s2c | pauses |
|---|---:|---:|---:|
| off | 6, 6, 6 | 3 | 0 |
| boundary | 17, 17, 17 | 9 | 9 |
| header (3 bytes) | 21, 21, 21 | 18 | 9 |
| payload (7 bytes) | 17, 17, 17 | 12 | 9 |

Partial-read proof: during the first pause of the BEGIN `C` message (len 11),
the proxy had written 3 bytes (header) or 7 bytes (payload) in one write; the
target task was suspended on a pending future, client `recv_q=0`,
`PQisBusy=true`, transaction ACTIVE.  This is a partial read followed by
suspension, not a TCP-segmentation claim.

Full header ladder, 21/21 cuts realised:

| k | phase | cut location | verdict | server | client |
|---|---|---|---|---|---|
| 1-5 | enter | no frame / `C` 3/11 / `Z` 3/6 | Neither | idle in transaction | INTRANS |
| 6-16 | body | complete `Z` and `1/2/n/C/Z` prefixes | Pre | idle | IDLE |
| 17-21 | exit_commit | complete `Z` / `C` 3/12 / `Z` 3/6 | Post | idle | IDLE |

Outcomes **aggregate by message phase**; half-frame cuts did not reveal a new
server→client responsibility state.  This does not claim message-level
completeness: one split, one pause length, one target shape, one cancellation.
The experimental entry monkeypatch had K=21 3/3 and entry cuts k=1-3 all
`Pre`/IDLE; this is not the actual submitted patch.

**New defect (send direction).** With an 8 MB parameter and the proxy
forwarding at most 64 KiB then not reading, the client was verified suspended
with `SIOCOUTQ` 3.1-4.1 MB, `PQflush=1`, connection ACTIVE.  Cancelling there
raises **`TypeError: an integer is required`**, not `CancelledError`; the
connection stays ACTIVE/busy and every successor gets `another command is
already in progress`.  A plain asyncio task reproduces it.  Located in
`psycopg/connection_async.py` wait/cancel re-entry ->
`waiting.wait_async(gen)` -> `next(gen)` on an already-started send generator.
A protocol-level positive control (libpq cancel+drain) passed 9/9, so the
harness does not force breakage.

The runtime containment (`partial_send_fix.py`) is retained as an experiment.
The source-level repair is on a separate local branch `codex/partial-send-cancel`
(commit `fbcbf651`): at cancellation, if `PQflush() != 0` the async connection
closes instead of resuming the suspended send generator; fully sent requests
keep the existing cancel/drain path; a second interruption also closes.  Five
real-server regression tests pass on the C and pure-Python backends, while
upstream fails with `TypeError` (C) or commits the cancelled INSERT (Python).
The repair is fail-closed and loses the lease; sync and pipeline partial sends
remain uncovered.  No upstream contact or novelty claim.

## 7. Ordinary failure vs one-sided interruption (group 4)

`failure_compare/`; details in
[`python/pg-mysql/failure_compare/RESULTS.md`](python/pg-mysql/failure_compare/RESULTS.md).
This matrix compares the **baseline** drivers' ordinary errors with one-sided
cancellation; the actual repairs are tested separately above. 60 new trials
(3 reps x 10 arms x psycopg/asyncpg), 112 s wall, 0 harness errors, 0 unrealised; real `Task.cancel` at calibrated entry/body/exit, real
`1/0` 22012, immediate/deferred unique 23505, failed SAVEPOINT 25P02,
`pg_terminate_backend` 57P01, plus reused sqlx future-drop and sync-signal data
(clearly labelled; not equivalent mechanisms).

- **Body:** cancellation and ordinary errors both roll back cleanly; successor
  visible.  No cancellation-specific body difference.
- **Entry:** cancellation leaves a healthy `INTRANS` transaction with no active
  block owner; both drivers' next CM returns normally while its write is never
  committed (3/3).  Ordinary entry failure is loud (`25P02`) or has a live
  owner.  psycopg's counter/cleanup dead-end is shared with ordinary failed
  entry; asyncpg's ownerless `_top_xact` is cancellation-specific.
- **Exit:** cancellation can leave the target **committed/visible** while the
  caller sees only `CancelledError` (3/3 both); psycopg locally says COMMITTED,
  asyncpg FAILED despite the commit.  Ordinary deferred-23505 is loud and rolls
  back.  Verify after a cancelled commit; do not assume rollback.
- **Common ordinary-failure hazard:** catch a body error then `COMMIT` reports
  success/COMMITTED on both drivers, but the target is invisible 3/3.  This is
  a cleanup omission, not specific to one-sided cancellation.
- **Reuse:** sync `entry_wait` matches psycopg entry; sync `exit_wait` matches
  exit-commit; sqlx BEGIN future-drop shows the same silent-successor shape,
  removed by PR #4394.  Sync pre-send/exit-pre-commit and a commit-reply-only
  Rust arm remain unrealised/Unknown.

## 8. What this says about responsibility handoff

The failures are not one generic "lost responsibility" shape:

1. **Entry half-transaction.** Server owns an open transaction, client has no
   usable owner.  Effect may be invisible; a naive retry can silently join the
   abandoned transaction.  Fixes: rollback inside cancellation (actual psycopg),
   clear the stale local registration and explicitly refuse the transaction API
   while requiring reset/rollback/close for ordinary reuse (actual asyncpg),
   close/poison (actual MySQL, but no usable successor).
2. **Exit commit ambiguity.** Server committed; caller cancelled.  This is a
   legitimate outcome, not automatically a bug, but the caller cannot infer it
   and local status can disagree with the server.  Needs post-cancel
   verification semantics.
3. **Partial send.** Client has not finished handing the request to the
   server; cancellation resumes a started generator and corrupts the wait
   state (C) or executes the cancelled statement (Python).  The source-level
   fix closes when libpq still has buffered output; reuse is not claimed.
4. **Stale reply.** MySQL's caller can return before the held reply; recovery
   may consume that reply as its own result.  Fast cancellation return is not
   evidence of a clean handoff.
5. **Delivery window.** Mature pools already protect assignment-through-
   delivery; the remaining hazard is the lease-use window after delivery.

Costs: the experimental in-place repair can add a full reply round trip
(psycopg 10 s at 5 s delay; nested 15 s); eviction is fast (sub-ms to ~0.37 ms)
but successor acquisition can be 20-50 s under the same delay; re-cancellation is a fast
escape but is a second interruption, not a repair; background isolation was
not tested in this round (only separate-process helpers), so no queued-worker
occupancy claim is made.

## 9. Final five answers

1. **When does old/new handle comparison work?** It works for a reused handle
   after entry cancellation, for clean body/exit handles, for fail-closed
   closure, and for pool return/reborrow. The state-only mode detects an open
   transaction; the effect mode detects an invisible write or an explicit
   transaction-API refusal. It does **not** add a new defect discovery over the
   complete calibrated differential on these targets. It needs declared
   semantics for legitimate outer transactions, read-only/session-blocked
   handles and committed exit cuts; timeout/blocked is unprobeable, not clean.
   Same-checkout retries can be hidden by a pool reset.
2. **Actual cut scale and runtime?** K=6 psycopg, 3 asyncpg, 12 nested
   psycopg, 4 warm pool checkout; K stable over 5 runs but not exhaustive.
   Counts + all cuts: 13.2 s, 10.7 s, 24.2 s, 1.8 s. The complete differential
   with two reps and calibration took 24.7-25.2 s (psycopg) and 16.4-16.5 s
   (asyncpg); the effect-only ablation took 12.8-13.3 s and 12.9 s; the probe took
   20.6-26.2 s and 13.0-15.8 s for the same cuts and reps. The nested target,
   not the single transaction, is the realistic lower bound for a complex
   operation.
3. **Confirmed increment over a strong manual baseline?** Confirmed: the
   complete differential finds the psycopg entry defect (4/4 `Neither`) and the
   asyncpg entry/commit defects; the actual psycopg patch removes its entry
   violation; the probe classifies invisible write vs transaction-API refusal
   vs pending outer vs blocked session and gives a fresh-reference control;
   targeted entry testing matches exhaustive cutpoint discovery at lower
   runtime; exhaustive exploration found no second entry/body/exit root cause
   here; half-frame splitting raises K but only reproduces phase states. The
   probe's discovery increment over the complete differential is zero.
   Unconfirmed: general superiority, message-level completeness, or a generic
   probe for arbitrary session/transaction semantics.
4. **New responsibility-handoff result?** Mature pool delivery is protected
   (no leak/capacity loss). The actual asyncpg registration patch does **not**
   make same-lease ordinary reuse safe (invisible `INSERT 0 1`); it refuses the
   transaction API explicitly and preserves a legitimate outer transaction,
   while the older terminate arm kills the outer transaction on a nested
   cancellation. The actual psycopg entry patch rolls back and yields a clean
   successor. MySQL's actual patch closes/poisons fast but does not yield a
   usable successor without reconnect. A psycopg partial-send cancellation
   defect (`TypeError`/stuck ACTIVE in C, committed cancelled INSERT in Python)
   now has a source-level fail-closed repair with failing-baseline/passing-fixed
   tests.
5. **Most worthwhile next mainline?** Carry the "closed vs reusable" contract
   into the probe and exercise it on the actual patches, rather than adding
   more generic cutpoints. The partial-send repair is implemented locally but
   still needs sync/pipeline coverage and upstream review; the asyncpg patch's
   same-lease ordinary-operation hazard is a caller-recovery contract, not
   something the registration patch claims to solve.

## 10. Artifact index

- Handle probe: [`successor_probe/`](python/pg-mysql/successor_probe/) — `probe.py`, `run_pg.py`, `run_pool.py`, logs, `RESULTS.md`.
- Cut scale / methods: [`method_compare/`](python/pg-mysql/method_compare/) — `cut_scale.py`, `effect_judge.py`, `complete_judge.py`, `logs/complete/`, `logs/effect-actual/`, `RESULTS.md`.
- Pool delivery: [`pool_handoff/`](python/pg-mysql/pool_handoff/) — `delivery_case.py`, logs, `RESULTS.md`.
- asyncpg same-lease: [`postfix_lease/`](python/pg-mysql/postfix_lease/) — `asyncpg_lease.py`, logs, `RESULTS.md`.
- psycopg delay/recovery: [`delay_recovery/`](python/pg-mysql/delay_recovery/) — `psycopg_delay.py`, `pool_capacity.py`, logs, `RESULTS.md`.
- MySQL delay/recovery: [`mysql_delay/`](python/pg-mysql/mysql_delay/) — `delay_proxy.py`, `case.py`, `run.sh`, 75 records, `RESULTS.md`.
- Fragmentation/backpressure: [`fragmentation/`](python/pg-mysql/fragmentation/) — `fragment_proxy.py`, `run_small.py`, `min_repro.py`, `partial_send_fix.py`, logs, `RESULTS.md`; source-level fix on `/home/ava/Worktrees/psycopg/partial-send` branch `codex/partial-send-cancel` commit `fbcbf651`.
- Ordinary vs cancellation: [`failure_compare/`](python/pg-mysql/failure_compare/) — `compare.py`, 60 trials, reused future-drop logs, `RESULTS.md`.
- Regression tests: `tests/test_round5_logic.py` plus the existing suite; all 32 tests pass.
