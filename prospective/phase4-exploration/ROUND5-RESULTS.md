# Round 5 results: handle comparison, cut scale, delivery handoff, and four appended groups

New work only; round-4 results and the user's existing logs are not rewritten.
All experiments use disposable local databases and unique tables.  The main
artifacts are under `python/pg-mysql/`; commands and raw JSONL are linked from
each sub-report, not duplicated here.

## 0. Short answer

- The old/new handle comparison **works** for the entry-cancellation defect:
  it detects an open server transaction and an invisible marker that an
  effect-only differential misses.  It **fails or needs extra semantics** for
  legitimate outer transactions, read-only/session-blocked handles, committed
  exit cuts, and pool reset/reborrow (a clean next checkout can hide a bad
  same-checkout retry).
- The measured cut scale is 6 (psycopg direct), 3 (asyncpg direct), 12
  (psycopg outer+inner savepoint), and 4 (warm pool checkout); nested operation
  is the main underestimate of a "transaction boundary".  Automatic runtime
  is ~1.8-24 s per target for counts + all cuts; target-specific setup is the
  main authoring cost.
- Confirmed increment over a strong manual baseline: the probe finds the entry
  contamination; exhaustive Dropwise cutpoint search finds **no additional
  root cause** on these targets; an effect-only differential misses the entry
  defect and false-flags legitimate commit-after-cancel.  The new increment
  came from a **different axis** (send backpressure), not from more cutpoints.
- The pool delivery window is correctly protected in psycopg_pool/asyncpg;
  no leak or capacity loss was found.  A same-checkout post-cancel reuse can
  still silently lose a write, so the delivery window and the lease-use window
  are separate.
- Most worthwhile next mainline: fix the newly reproduced psycopg cancellation
  path for a partly written request (TypeError / stuck ACTIVE connection), then
  express the post-cancel lease contract (closed vs reusable) in the probe.

## 1. Old/new handle comparison (Work 1)

`successor_probe/probe.py` + `run_pg.py` + `run_pool.py`;
details in [`python/pg-mysql/successor_probe/RESULTS.md`](python/pg-mysql/successor_probe/RESULTS.md).

Contract: a fresh nonce and column metadata must echo; the old handle gets a
unique marker write in a normal transaction and bounded independent visibility;
then it is closed/returned and the marker is re-read.  The reference is a fresh
direct handle.  The probe compares handle behaviour, not the target's intended
effect.

| target | old | reference | compare |
|---|---|---|---|
| psycopg entry k=1,2 | open_transaction | clean | **old_anomaly** |
| psycopg body/exit | clean | clean | consistent_clean |
| psycopg fix entry | clean | clean | consistent_clean |
| asyncpg entry k=1 | open_transaction | clean | **old_anomaly** |
| asyncpg fix entry | closed | clean | **old_closed** |
| legit outer/nested | open_transaction_allowed | clean | legitimate_open_transaction |
| session k=3 | clean | clean | consistent_clean |
| read-only k=3 | blocked_by_session | clean | old_not_applicable |

Why this is not just an effect check: at the psycopg entry cut the server is
`idle in transaction`, a retry through the official CM returns normally, and
the independent observer sees no row.  The effect-only judge (below) reports
`no_effect` and misses it.  The probe catches the open transaction and the
invisible marker.

Where it needs extra semantics:

- An open transaction is only an anomaly under the declared handoff contract;
  a legitimate outer transaction must be allowed explicitly.
- `blocked_by_session` / timeout is **unprobeable**, not clean.
- A committed exit cut looks clean and visible; the probe cannot know whether
  that commit was the intended effect or a legitimate cancellation-after-commit.
- A pool reset/reborrow can repair the handle before the next checkout; this
  does not prove a retry **inside** the same checkout was safe.
- The reference is direct while the tested handle goes through the delay proxy;
  it is a same-server fresh-handle baseline, not a byte-identical path.

## 2. Cut scale and method increment (Work 2)

`cut_scale.py` and `effect_judge.py`; details in
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

Method comparison on the same entry defect (targeted test not weakened):

| method | discovery | misses / false positives | runtime |
|---|---|---|---|
| targeted phase test | entry open transaction on both | none in sample | psycopg 1.86 s; asyncpg 0.83 s |
| Dropwise all-cut exploration | same entry defect | no additional root cause; overrun unrealised | psycopg 6.4 s (cuts) + 5.2 s (counts); asyncpg 3.0 s + 5.7 s |
| effect-only differential | **misses entry** | psycopg flags 2 exit cuts `visible_after_cancel` (legitimate commits); asyncpg 0 | psycopg 6.43 s; asyncpg 3.03 s |
| old/new handle probe | entry old_anomaly; fixed clean/closed | no false positives on body/exit/legit outer | ~1.7 s/cut psycopg; ~2.1 s/cut asyncpg |

Confirmed increment: the probe and the targeted test both find the entry
contamination; all-cut exploration does not find a second entry/body/exit
defect.  Unconfirmed: general superiority, message-level cut completeness, or
replacement of semantic phase knowledge.  The probe's advantage is
**discrimination** (open transaction vs clean/closed), not more cutpoints.

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
one-slot pool for reset and reborrow.  Two reps, original vs fail-closed patch.

| mode | original | patched |
|---|---|---|
| normal | committed/visible | same |
| entry same-lease INSERT | returns `INSERT 0 1`, observer sees **0 rows**; after return 0 | **explicit refusal** (`released back to the pool` / `connection is closed`); after return 0 |
| entry reset then INSERT | visible/committed | reset refused (already released) |
| entry transaction API | returns ok, write invisible | transaction API refused |
| body cancel | visible | visible |

So the original is **uncommitted success** (a caller trusting the return loses
the write), while the patch is **explicit refusal** (fail-closed; the pool
replaces the lease).  `_top_xact = None` was not server IDLE: the observer saw
`BEGIN` and `idle in transaction` at the cut.  The pool return/reset boundary is
clean in both; the same-checkout boundary is the hazard.

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
| fail-closed fix | **10.00 s** | cleared 0.0005 s | 10.01 s |
| fix + second cancel | **0.10 s** | cleared 0.0007 s | 0.11 s |
| nested original rollback | 5.00 s | outer exit `OutOfOrderTransactionNesting`, not cleared | — |
| nested fix | 15.01 s | cleared; outer rollback ok | 20.02 s |

Cancellation delivery is bounded by the delayed BEGIN reply, so an explicit
post-catch recovery cannot start earlier.  The fix rolls back inside the
cancelled entry, adding a second reply round trip.  A second cancel is an
escape (fast, closes), not a normal repair.  In a one-slot pool, close/replace
forces a new connection: successor 20.01 s; the fix retains the connection and
gives an immediate successor at the price of 10.00 s cancellation.

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
- Patched direct close evicts at cancel (0.14-0.37 ms) and eliminates the
  stale-reply false OK, but recovery fails immediately and **c is unrealised**.
  A usable successor needs reconnect: ~0.35 s at 50 ms delay, **30-35 s at
  5 s**, with c ~0.5 s / **45-50 s**.  Direct close is not O(1)/zero-wait to a
  usable successor.
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
The fixed variant (existing experimental repair) had K=21 3/3 and entry cuts
k=1-3 all `Pre`/IDLE.

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

A narrow runtime containment (`partial_send_fix.py`) closes the connection when
that exact TypeError escapes and re-raises `CancelledError`: the caller gets
the interruption, the successor is explicitly `connection is closed` rather
than stuck, at the cost of losing the lease.  This is fail-closed containment,
not an upstream-ready fix; no upstream contact or novelty claim.

## 7. Ordinary failure vs one-sided interruption (group 4)

`failure_compare/`; details in
[`python/pg-mysql/failure_compare/RESULTS.md`](python/pg-mysql/failure_compare/RESULTS.md).
60 new trials (3 reps x 10 arms x psycopg/asyncpg), 112 s wall, 0 harness
errors, 0 unrealised; real `Task.cancel` at calibrated entry/body/exit, real
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
   abandoned transaction.  Fixes: rollback inside cancellation (psycopg),
   terminate/refuse (asyncpg), direct close (MySQL, but no usable successor).
2. **Exit commit ambiguity.** Server committed; caller cancelled.  This is a
   legitimate outcome, not automatically a bug, but the caller cannot infer it
   and local status can disagree with the server.  Needs post-cancel
   verification semantics.
3. **Partial send.** Client has not finished handing the request to the
   server; cancellation resumes a started generator and corrupts the wait
   state.  Fail-closed close is safe; reuse is not.
4. **Stale reply.** MySQL's caller can return before the held reply; recovery
   may consume that reply as its own result.  Fast cancellation return is not
   evidence of a clean handoff.
5. **Delivery window.** Mature pools already protect assignment-through-
   delivery; the remaining hazard is the lease-use window after delivery.

Costs: in-place repair can add a full reply round trip (psycopg 10 s at 5 s
delay; nested 15 s); eviction is fast (sub-ms to ~0.37 ms) but successor
acquisition can be 20-50 s under the same delay; re-cancellation is a fast
escape but is a second interruption, not a repair; background isolation was
not tested in this round (only separate-process helpers), so no queued-worker
occupancy claim is made.

## 9. Final five answers

1. **When does old/new handle comparison work?**  It works for a reused handle
   after entry cancellation, for clean body/exit handles, for fail-closed
   closure, and for pool return/reborrow.  It detects an open transaction and
   an invisible marker that an effect-only differential misses.  It fails or
   needs declared semantics for legitimate outer transactions, read-only/
   session-blocked handles, committed exit cuts, and same-checkout retries
   hidden by a pool reset; timeout/blocked is unprobeable, not clean.
2. **Actual cut scale and runtime?**  K=6 psycopg, 3 asyncpg, 12 nested
   psycopg, 4 warm pool checkout; K stable over 5 runs but not exhaustive.
   Counts + all cuts: 13.2 s, 10.7 s, 24.2 s, 1.8 s.  The nested target, not
   the single transaction, is the realistic lower bound for a complex
   operation.
3. **Confirmed increment over a strong manual baseline?**  Confirmed: the probe
   discriminates entry contamination and fail-closed closure where effect-only
   fails; targeted entry testing matches exhaustive cutpoint discovery at about
   one third of the cut runtime; exhaustive exploration found no second
   entry/body/exit root cause here; half-frame splitting raises K but only
   reproduces phase states.  Unconfirmed: general superiority, message-level
   completeness, or a generic probe for arbitrary session/transaction
   semantics.
4. **New responsibility-handoff result?**  Mature pool delivery is protected
   (no leak/capacity loss); asyncpg entry-fix same-lease reuse is explicitly
   refused, while the original silently loses the write; psycopg entry fix
   protects top-level and nested entry at a latency cost; MySQL direct close
   evicts fast but does not yield a usable successor without reconnect; and a
   new psycopg partial-send cancellation defect (`TypeError`, stuck ACTIVE) was
   reproduced and narrowly contained.
5. **Most worthwhile next mainline?**  Fix/verify the psycopg wait/cancel-
   generator path for partial sends, with a before/after contract test
   (`CancelledError`, connection closed or provably drained, successor
   explicit), then carry that "closed vs reusable" result into the handle
   probe.  This is a concrete, reproducible defect on the same responsibility
   boundary; it is more valuable than adding more generic cutpoints.

## 10. Artifact index

- Handle probe: [`successor_probe/`](python/pg-mysql/successor_probe/) — `probe.py`, `run_pg.py`, `run_pool.py`, logs, `RESULTS.md`.
- Cut scale / methods: [`method_compare/`](python/pg-mysql/method_compare/) — `cut_scale.py`, `effect_judge.py`, logs, `RESULTS.md`.
- Pool delivery: [`pool_handoff/`](python/pg-mysql/pool_handoff/) — `delivery_case.py`, logs, `RESULTS.md`.
- asyncpg same-lease: [`postfix_lease/`](python/pg-mysql/postfix_lease/) — `asyncpg_lease.py`, logs, `RESULTS.md`.
- psycopg delay/recovery: [`delay_recovery/`](python/pg-mysql/delay_recovery/) — `psycopg_delay.py`, `pool_capacity.py`, logs, `RESULTS.md`.
- MySQL delay/recovery: [`mysql_delay/`](python/pg-mysql/mysql_delay/) — `delay_proxy.py`, `case.py`, `run.sh`, 75 records, `RESULTS.md`.
- Fragmentation/backpressure: [`fragmentation/`](python/pg-mysql/fragmentation/) — `fragment_proxy.py`, `run_small.py`, `min_repro.py`, `partial_send_fix.py`, logs, `RESULTS.md`.
- Ordinary vs cancellation: [`failure_compare/`](python/pg-mysql/failure_compare/) — `compare.py`, 60 trials, reused future-drop logs, `RESULTS.md`.
- Regression tests: `tests/test_round5_logic.py` plus the existing suite; all 29 tests pass.
