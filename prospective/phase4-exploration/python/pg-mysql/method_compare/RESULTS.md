# Cut scale and method increment (round 5, Work 2)

> Review correction (2026-10-22): `effect_judge.py` is a target-only ablation, not the existing calibrated successor differential. Its `visible_after_cancel` label is not an Atomic violation. The stated superiority/false-positive comparison against the original differential method is not established; a same-contract comparison remains required. Entry probe detections also currently short-circuit on transaction state before the write probe.

All numbers below are from the disposable local PostgreSQL 18.4 at
`127.0.0.1:32870`, Python 3.13.13, psycopg 3.3.6, asyncpg 0.32.0.  The tested
handle goes through a 50 ms per-chunk `DelayProxy`; the observer is a separate
direct connection.  Each count starts from a fresh scenario/table.  Logs:
`method_compare/logs/`.

## 1. What was measured

`cut_scale.py` separates four quantities that are often conflated:

- **K**: number of times the target task actually suspended on the normal path
  (the Pending/yield count), measured over 5 uncancelled runs;
- **source awaits**: lexical `await` expressions in the harness functions,
  extracted with `ast` and their source lines;
- **cutpoints**: realised cuts at every `k` in `1..K`, plus one overrun request
  `K+1`;
- **trial repeats**: 5 normal runs and 1 trial per cut (not a scheduling
  sweep).

At every suspension it records the coroutine await chain (file/line/function),
so a cut can be mapped to the source line and the scenario phase.  A single
trace is not exhaustive scheduling; repeated K values are reported as
variation, not coverage.

## 2. Cut scale

| target | K (5 runs) | source awaits | realised cuts | overrun K+1 | phases at cuts | wall time (5 counts + all cuts) |
|---|---:|---:|---:|---:|---|---:|
| direct psycopg, transaction CM | 6, all runs | 5 | 6/6 | unrealised | enter 2, body 2, exit_commit 2 | 13.23 s |
| direct asyncpg, transaction CM | 3, all runs | 5 | 3/3 | unrealised | enter 1, body 1, exit_commit 1 | 10.65 s |
| psycopg outer + inner savepoint + two writes | **12**, all runs | 4 | 12/12 | unrealised | outer_enter 2, outer_body 2, inner_enter 2, inner_body 4, outer_exit 2 | 24.22 s |
| psycopg pool checkout wrapped as target | 4, all runs | 2 | 4/4 | unrealised | enter 1, body 1, body_wait 2 | 1.80 s |

Key observations:

- K is stable across the 5 repeats for all four targets; no variation was
  observed.  This is a small repeat sample, not a claim about all schedules.
- **K is not the source-await count.**  psycopg and asyncpg have the same five
  lexical awaits in `_cm_write`; psycopg yields twice per send/reply exchange
  (6 cutpoints), asyncpg once (3).  The conditional rollback await is present
  in the source but not observed on the normal path, so it is listed separately
  rather than counted as an implemented normal-path cutpoint.
- The overrun request `K+1` is unrealised because the target already completed.
  It is not an error and not an unimplemented cutpoint.
- The nested target raises K from 6 to 12 without a new API call: the same
  transaction CM is used twice (outer + inner).  This is the strongest evidence
  that a "transaction boundary" is not one cutpoint.
- The pool-checkout target had a warm connection, so the checkout did not add a
  handoff cutpoint; K=4 all landed in the transaction.  That is a limitation of
  this sample, not a statement that a pool delivery window has no cutpoints.
  Work 3 (`pool_handoff/`) drives the delivery window separately.

The per-cut source locations are in the JSONL.  For psycopg, cuts 1/2 are both
at `tx.__aenter__` (`txphase_case.py:421`), 3/4 at the body execute (`:430`),
5/6 at commit `__aexit__` (`:452`).  For asyncpg, one cut per phase.  For the
nested target, the phase map is explicit in the table above.

## 3. Method comparison on the same entry defect

The target is the known round-4 psycopg/asyncpg cancelled transaction entry
(server executed `BEGIN`, client waits on the reply, then cancellation).  All
methods see the same target, effect criterion and comparable budget; the
targeted test is not deliberately weakened.

| method | cuts used | discovery | false positives / Unknown | measured runtime | authoring/reuse |
|---|---|---|---|---|---|
| targeted phase test | psycopg 1,2; asyncpg 1 (entry) | open transaction / server `INTRANS` on both; finds the defect | 0 in this sample | psycopg 1.86 s; asyncpg 0.83 s (setup+cut) | needs a phase map or source knowledge; reuses the scenario |
| Dropwise cutpoint exploration | all K: psycopg 6, asyncpg 3 | same entry defect; **no additional root cause** | 0; overrun unrealised | psycopg 6.4 s cuts (+5.2 s counts); asyncpg 3.0 s (+5.7 s counts) | reuses engine; no target-specific branch |
| effect-only differential | all K | **misses entry**: target cancelled, no row, verdict `no_effect` | psycopg flags both exit cuts as `visible_after_cancel` (2; legitimate commit-after-cancel); asyncpg 0 | psycopg 6.43 s; asyncpg 3.03 s | simplest to write, but conflates "no effect" with "clean" |
| old/new handle probe | entry cuts targeted (2 psycopg, 1 asyncpg); all K available | old=`open_transaction`, ref=`clean`, `old_anomaly`; fixed psycopg `clean`, fixed asyncpg `old_closed` | blocked/session/timeout are not anomalies; no false positive on body/exit/legit outer | ~1.7 s/cut psycopg, ~2.1 s/cut asyncpg (2.0-2.5 s including setup) | reuses scenario; needs probe contract and reference handle |

Effect-only detail (psycopg): k=1..4 `no_effect`, k=5/6
`visible_after_cancel`.  The latter are **not** bugs: the target had already
committed, and cancellation arrived after the effect.  A naive effect-only
judge would report two false positives and miss the real entry contamination.
Asyncpg's k=3 happened to land before commit in the effect-only run, so it
reported `no_effect`; the probe run at k=3 saw the row visible.  That is
run-to-run timing variation at the same K, not a stable phase distinction.

## 4. What the comparison establishes

Confirmed in this sample:

- The old/new handle probe detects the entry defect that an effect-only
  differential misses, and distinguishes `old_closed` (fail-closed repair) from
  contamination.
- A targeted test at the known entry phase finds the same defect as exhaustive
  cutpoint exploration, at about one third of the psycopg cut cost and one
  third of the asyncpg cut cost.
- Exhaustive exploration did **not** find a second entry/body/exit defect in
  these targets; its extra cuts are mostly more waiting boundaries inside the
  same message phase, not new responsibility states.
- Nested operation raises the cut scale substantially (12 vs 6) even though the
  API surface is the same; the single-transaction K understates it.

Not established:

- That message-level cutpoints are complete, or that a single trace covers all
  schedules.  No such claim is made.
- That the probe can replace semantic knowledge for legitimate outer
  transactions, session state or asynchronous commit ambiguity.  The outer
  probe needed an explicit `open_transaction_allowed`; read-only session state
  made the write probe `blocked_by_session` (unprobeable, not clean), and exit
  commit remains ambiguous to the caller even when the probe says clean.
- That cutpoint exploration is generally superior or inferior.  On this target
  it had no discovery increment over a well-placed test; on a target without a
  known phase it still searches where the author did not look.

## 5. Cost notes

Wall times include setup and cleanup; they are automatic runtime, not human
effort.  The new code is `cut_scale.py` (~350 lines) and `effect_judge.py`
(~90 lines); the probe and scenarios already existed.  The main adaptation
work was making the proxy delay configurable, separating observer/tested
application names, and avoiding a timed-out pool acquire that would poison the
successor measurement.  No retroactive time estimate is claimed.
