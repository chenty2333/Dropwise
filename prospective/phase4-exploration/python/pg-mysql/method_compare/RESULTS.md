# Cut scale and method comparison (round 5, Work 2 — corrected)

> Correction (2026-10-22): `effect_judge.py` is a **target-effect-only
> ablation**, not the original calibrated successor differential. Its
> `visible_after_cancel` label is not an Atomic violation. The earlier claim
> that the new probe uniquely finds a defect the original differential misses
> is withdrawn. The complete method was run on the same target and cutpoints;
> it finds the psycopg entry defect. The probe's remaining increment is
> classification, not discovery.

All numbers are from the disposable PostgreSQL 18.4 on `127.0.0.1:32919`,
Python 3.13.13, psycopg 3.3.6 (baseline) or the actual patch worktree
(`/home/ava/Worktrees/psycopg/tx-cancel`, `AsyncTransaction._recover_entry_failure`
present), asyncpg 0.32.0 (baseline) or `/home/ava/Worktrees/asyncpg/repo`
(HEAD `ff1bb8d`, top-xact clear present). Each `COMPLETE-CALIBRATION` line now
records the loaded package path/version and the repair marker; `--repair actual`
refuses to run if the marker is absent. The tested handle goes through a 50 ms
per-chunk `DelayProxy`; the observer is direct. Logs:
[`logs/complete/`](logs/complete/), [`logs/effect-actual/`](logs/effect-actual/),
and `../successor_probe/logs/actual-matrix/`.

## 1. Cut scale (retained from the original run)

| target | K (5 normal runs) | source awaits | realised cuts | phases | wall time (counts + cuts) |
|---|---:|---:|---:|---|---:|
| direct psycopg CM | 6, all | 5 | 6/6 | enter 2, body 2, exit 2 | 13.23 s |
| direct asyncpg CM | 3, all | 5 | 3/3 | enter 1, body 1, exit 1 | 10.65 s |
| psycopg outer + inner savepoint | **12**, all | 4 | 12/12 | outer_enter 2, outer_body 2, inner_enter 2, inner_body 4, outer_exit 2 | 24.22 s |
| psycopg pool checkout | 4, all | 2 | 4/4 | enter 1, body 1, body_wait 2 | 1.80 s |

K is stable over five repeats but is not exhaustive scheduling. K is not the
source-await count: psycopg yields twice per exchange (6), asyncpg once (3).
The pool sample was warm, so no checkout handoff cutpoint appeared. Nested
operation is the main underestimate of a "transaction boundary".

## 2. Complete differential (the original calibrated method)

`complete_judge.py` calls `dropwise_py.engine.explore_observed()` with
`Expect.ATOMIC`: two no-target pre samples, two completed-target post samples,
then per cut a survivor write, an independent observer read, and a
Pre/Post/Mixed/Neither verdict. `Neither` is an Atomic violation. The survivor
uses the same official transaction context manager as the target; it is not a
weakened effect-only check.

| run | cuts | reps | verdicts | violations | wall |
|---|---:|---:|---|---:|---:|
| psycopg baseline | 1-6 | 2 | 4 Neither (k=1,2), 4 Pre (k=3,4), 4 Post (k=5,6) | **4** | 24.79 s |
| psycopg actual patch | 1-6 | 2 | 8 Pre, 4 Post | **0** | 25.24 s |
| asyncpg baseline | 1-3 | 2 | 4 Neither (k=1 and k=3), 2 Pre (k=2) | **4** | 16.44 s |
| asyncpg actual patch | 1-3 | 2 | 2 Neither (k=3), 4 Pre (k=1,2) | **2** | 16.43 s |

The psycopg result is unambiguous: the baseline entry cuts leave an abandoned
transaction, so the survivor's commit is only a savepoint release; observation
is neither pre nor post. The actual patch recovers entry (Pre) and removes the
violation.

The asyncpg result needs the boundary stated carefully:

- Baseline `k=1` is entry contamination (Neither). Baseline `k=3` is a
  **commit-in-flight** ambiguity: the target was cancelled at exit, the server
  transaction was still open, and the survivor was refused; that is a real
  violation of the survivor contract, not entry contamination.
- Actual patch `k=1` was **Pre** in this sample: cancellation had not reached
  the server's `BEGIN`, the clear was enough, and the survivor ran as a real
  top-level transaction. A separate observer-confirmed-`BEGIN` lease run (see
  `postfix_lease/`) lands after `BEGIN`; then the same handle reports
  `transaction_api_error` and the state diagnosis is `open_transaction`. The cut
  index does not pin the wire phase.
- Actual patch `k=3` still violates: a cancellation during `COMMIT` can leave
  the transaction open/unknown, and the patch does not clear or roll it back.
  This remains caller recovery (reset/rollback/close), not something the
  registration patch claims to solve.

## 3. Effect-only ablation

`effect_judge.py` observes only the target row and the target's returned
outcome. It does not run a successor, does not calibrate pre/post, and has no
Atomic contract. Its labels are descriptive, not verdicts.

Two reps per cut, matching the complete and probe runs:

| run | verdicts (2 reps) | wall |
|---|---|---:|
| psycopg baseline | 8 `no_effect` (k=1-4), 4 `visible_after_cancel` (k=5,6) | 12.84 s |
| psycopg actual patch | 8 `no_effect`, 4 `visible_after_cancel` | 13.26 s |
| asyncpg baseline | 6 `clean_completed` (k=2,3), 6 `no_effect` (k=1) | 12.89 s |
| asyncpg actual patch | 6 `clean_completed`, 6 `no_effect` | 12.91 s |

It **misses** the psycopg entry defect: the target row is absent at k=1,2, so
it reports `no_effect`, indistinguishable from a clean rollback at k=3,4. The
complete differential reports Neither at k=1,2 because the survivor's effect is
also missing. `visible_after_cancel` at k=5,6 is not an Atomic violation: the
target may have committed before the cancellation, and Atomic allows Post. The
ablation is useful as a cheap target-effect description; it is not a substitute
for the successor contract.

## 4. New probe, same cutpoints

| run | effect-mode old status | wall (2 reps) |
|---|---|---:|
| psycopg baseline | 4 `invisible_write` (k=1,2), 8 clean | 26.19 s |
| psycopg actual patch | 12 clean | 20.57 s |
| asyncpg baseline | 2 `invisible_write` (k=1), 4 clean | 15.78 s |
| asyncpg actual patch | 2 `transaction_api_error` (k=1), 4 clean | 12.96 s |

The probe uses the same inherited `CmScenario` target function and the same
cut list, but its setup also creates the two-table probe schema and a reference
handle. The target code is unchanged; the extra setup can change scheduling, so
a cut index is not a byte/packet position (the asyncpg `k=1` scheduling split
below is an example). The complete differential has its own pre/post
calibration runs, which the ablation does not.

The probe's black-box effect detects the same psycopg entry defect as the
complete differential. On asyncpg actual `k=1`, it reports an explicit
transaction-API refusal; the complete run at the same cut happened to cancel
before `BEGIN` and reported Pre. That is a wire-timing difference, not extra
discovery. At asyncpg `k=3`, the probe saw the target committed and reported
clean; the complete run saw the commit still in flight and reported Neither.
Neither method is "right" across both schedules; the phase label is not a wire
snapshot.

Discovery increment of the probe over the complete differential on this target:
**zero**. Judgment increment: it distinguishes `invisible_write` from
`transaction_api_error` from `effect_pending_outer` from
`blocked_by_session`, and its fresh reference confirms the anomaly is
handle-specific. That is useful for the repair contract, but it does not
replace the complete differential's survivor/Atomic verdict.

## 5. Cost and adaptation

| method | one run (psycopg 6 cuts / asyncpg 3 cuts) | per-trial | code/reuse |
|---|---:|---:|---|
| complete differential | 24.79-25.24 s / 16.43-16.44 s (2 reps + 4 calibration runs) | ~2.0 s / ~2.7 s | reuses `explore_observed` and the existing scenario; ~80-line runner |
| effect-only | 12.84-13.26 s / 12.89-12.91 s (2 reps) | ~1.1 s / ~1.1 s | ~100-line ablation; no successor/reference |
| new probe effect | 20.57-26.19 s / 12.96-15.78 s (2 reps) | ~1.7-2.2 s | ~250-line probe plus a reference connection and probe schema; needs the handoff contract and a fresh-handle control |
| targeted phase test | psycopg 1.86 s / asyncpg 0.83 s | one cut | needs phase knowledge or an observer-confirmed trigger |

The complete differential is not prohibitively expensive; its fixed calibration
(4 runs) is amortised over the cuts, and per-trial cost is comparable to the
probe. The effect-only ablation is cheapest but answers a different question.
The probe's main authoring cost is the contract and reference, not runtime.

## 6. What is established, and what is not

Established in this sample:

- The original complete differential detects the psycopg entry defect (4/4
  Neither at k=1,2) and the asyncpg baseline entry/commit defects; the actual
  psycopg patch removes its entry violation; the actual asyncpg patch removes
  the pre-`BEGIN` entry case but not the commit-in-flight case or the
  post-`BEGIN` refusal boundary.
- The effect-only ablation misses the entry defect and calls legitimate
  commit-after-cancel `visible_after_cancel`; it is not an Atomic judge.
- The new probe's black-box effect finds the same psycopg entry defect as the
  complete differential; its increment is classification and reference
  calibration, not discovery.
- Cut indices are not wire-phase identifiers. The same `k` can land before or
  after `BEGIN`/`COMMIT`; targeted observer-confirmed cuts are needed for
  boundary claims.

Not established:

- That any method is generally superior or that message-level cutpoints are
  complete.
- That the probe replaces semantic knowledge for legitimate outer
  transactions, session state or asynchronous commit ambiguity.
- That the actual asyncpg registration patch protects same-lease ordinary
  operations (it does not; see `../postfix_lease/`), or that the actual
  psycopg patch covers the partial-send path (it is a separate repair; see
  `../fragmentation/`).
