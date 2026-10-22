# Round 5: offline discovery — what was built, what changed, what is next

This is the entry point for the round-5 deliverable.  The detailed mechanism
report is [`docs/RESULTS-sync-signal.md`](docs/RESULTS-sync-signal.md); the
method report is [`docs/METHOD-fields.md`](docs/METHOD-fields.md); the harness
and reproduction instructions are in [`README.md`](README.md).

## Integration review (2026-10-22)

The synchronous prototype now holds the original connection lock throughout
entry/exit and repair. The former wrapper released it before reacquiring it,
allowing another thread to see broken state. Four regression tests check the
first-unlock boundary, successful paths, and fail-closed repair interruption.
This remains a bounded prototype, not arbitrary-signal safety.

Field judgment now rejects missing calibration fields rather than treating them
as observed `None`, rejects colliding dotted paths, and stops after a failed
initial baseline. Actual `None` values and nested calibration remain supported;
unrealized cuts remain unjudged. Six added tests cover these boundaries.

Independent rerun: Python suites 33 + 11 tests passed; the complete sync runner
finished with 60 signal trials, 48 calibrations and 72 interrupted trials per
original/fix matrix (no calibration failures or trial errors), original/fixed
pool cases, three SIGINT trials, the ownership contrast and 16 field trials.
The repaired B/D/S4 retry and survivor cases were visible 3/3 each; C rejected
reuse explicitly 3/3 per mode. All six fixed pool victims succeeded on the same
backend. Field replay reported 12 interrupted violations and 4 normal controls.
The signal sweep reached 22 clean, 8 pre-send, 12 unread-reply and 18 wait
states; these host-specific counts are not production probabilities.

Mainline update (same date): the field contract now also exists in the Rust
engine (`src/fields.rs`, `explore_fields`/`replay_fields`/`assert_fields`) with
per-field `Expect`, cross-field invariants, baseline/strict checks and
`FieldStatus` coverage gaps.  The Python logs below are unchanged and were not
re-judged by the Rust code; the port deliberately does not sample mixed
calibrations, so a fused aggregate can differ from the Python blob's `Mixed`
verdict.  The ownership witness is implemented in `prospective/online-guards`
as RAII claims (nested, owner-only recovery, handoff, forgotten-claim
retirement); it is not a psycopg patch.

The old records below remain historical runs; the rerun does not expand the
prototype's nested/pipeline/async scope. In this report, an invisible write is
uncommitted at the observation boundary, not proof of permanent loss before
rollback or connection closure.

## What was actually built and run

* **Sync psycopg harness** (`python/pgsync/`): a loopback PostgreSQL frame
  proxy with per-direction delay and JSONL logs; real `SIGALRM` and `SIGINT`
  injectors; deterministic source-line injection; an independent observer
  connection; client/server/wire state capture; recovery, retry, survivor, and
  pool matrices.  All writes go to per-trial tables in a disposable local
  container.
* **Candidate repair** (`pgsync/fix.py`): a monkeypatched sync
  `Transaction.__enter__`/`__exit__` that repairs B/S4/D, fails C closed, and
  re-raises the original interruption.  It is a research prototype, not an
  upstream patch.
* **Field-contract method** (`research/python-retro/dropwise_py/fields.py`):
  named `EFFECT`/`STATE` fields, per-field `Atomic`/`Completed`/`Undone`, and
  coverage statuses (`Uncovered`, `Degenerate`, `Unknown`).  `engine.py` is
  unchanged.  13 no-service tests; the same `judge_trial` is used by the async
  engine and the sync harness.
* **Measured results** (one host, Python 3.13.13, psycopg 3.3.6, PostgreSQL
  18-alpine): 60-trial real-signal sweep (21 clean, 12 pre-send, 9 unread-reply,
  18 wait); 3 trials/cell original/fix matrix; 3 pool trials per build; 3 real
  `SIGINT` trials; one legitimate-vs-abandoned contrast; field replay on three
  interrupted sync states plus a normal-path negative control.  Raw JSONL is in
  [`logs/2026-10-22/`](logs/2026-10-22/).
* **Runner** (`run-sync.sh`): starts its own throwaway PostgreSQL container,
  pins and verifies the five psycopg source lines, runs both unit suites, all
  experiments, and the analysis.  Tested end to end in this round.

## Delta vs the start (round-4 leads)

| round-4 lead | this round |
|---|---|
| psycopg sync `KeyboardInterrupt` at entry + sync pool return | done deeply: B/D/C/S4, real signal + deterministic points, recovery matrix, pool victim, fix contrast |
| asyncpg exit-commit distribution | not advanced; round-4 observations remain the reference |
| retry inside a realistic checkout | partially: same-checkout retry and pool next-checkout, not a web framework |
| mysql-connector cut-4 / prepared statements | not advanced |
| cross-driver entry-cancellation scenario | not advanced |
| upstream-quality psycopg patch + failing test | prototype + regression matrix, not upstream |

The highest-value lead (sync entry + pool) was taken to a closed loop: mechanism
→ real reachability → consequence → pool victim → fix → fix contrast → method.

## Hypotheses that were refuted or revised

* **“The sync pool's reset protects the next checkout.”**  Only for B.  In D
  the reset sees `IDLE` and returns the poisoned handle; the next transaction
  raises the bare assertion, and the next write is not committed.  The reset is
  a server-state check, not an ownership check.
* **“`conn.rollback()` is available after an interrupted entry.”**  No for B
  and D: the local counter is 1 and the public guard raises `ProgrammingError`.
  It is available for S4 (counter 0), so recovery permission is decided by
  local bookkeeping, not by the server's `INTRANS`.
* **“Raw SQL `ROLLBACK` repairs the handle.”**  Server-only: it clears the
  server transaction but leaves the local counter at 1; the next
  `conn.transaction()` still fails.  A reconnect (or non-public state reset) is
  required.
* **“The sync and async reachable states are the same.”**  B matches the async
  entry trap, but D and C are sync-only: asyncio cancellation is delivered at a
  suspension, and there is no suspension between `_push_savepoint` and
  `send_query`; a Python-level asynchronous exception can land there.
* **“A wire/kernel observer can tell legitimate nesting from an abandoned
  transaction.”**  Not from the facts available: both are `INTRANS`, both show
  `SAVEPOINT`/`RELEASE`, and the write is invisible in both.  The difference is
  whether a live owner will commit; that is not a packet or session-state fact.
* **“The unread-reply state can be repaired.”**  No: the generator is closed
  and libpq still owns a command; the fix closes the connection.

## Implemented vs model-only vs proposal

* **Implemented and measured:** the harness, injectors, observer, state
  taxonomy, recovery/pool matrices, the candidate repair, the field module and
  tests, the runner, the raw records.
* **Model/source argument only:** the async-vs-sync reachability difference
  (source reading + round-4 phase map, no new async run); the ownership
  argument in the legitimate-vs-abandoned contrast (constructed three-way
  comparison, not a production trace).
* **Proposal, not result:** an upstream psycopg patch with its test suite; an
  eBPF/kernel observation experiment (the negative result in §4.6 predicts it
  cannot recover ownership alone); auto-deriving fields from resource identity;
  tests on SQLAlchemy/anyio/pipeline targets not used in the design.
* **Ported outside this Python round:** the Rust field contract and the online
  ownership claims (see the mainline update above).  They are separate
  implementations with their own tests, not new measurements of this harness.

## Residual limits

One host and one library version; microsecond windows are host-specific; 3
trials/cell are existence measurements, not probabilities; `SIGALRM`/`SIGINT`
equivalence was checked on the wait window only; the deterministic injection is
synthetic (real reachability shown separately); the pool test used a
swallowed-interrupt path with pool size 1; the fix does not cover nested exit,
pipelines, `force_rollback`, two-phase transactions, or async; the field
contract is declared, not discovered, and assumes effect independence.

## Next most valuable direction

1. Turn the repair into an upstream-quality patch and decide whether the
   ownership fact belongs in psycopg or in the pool's reset callback; add the
   nested-entry/exit and non-autocommit cases.
2. Build an explicit live-owner witness and test it against the legitimate
   nested transaction in §4.6; measure false positives before any guard ships.
3. Auto-derive fields from resource identity in the Rust engine and re-run
   round-3/4 cases to see which “passes” were actually uncovered; the manual
   port is done, but its discovery claim is not yet measured on those cases.


## Closeout decision (2026-10-22)

The main contribution pursued next is cross-layer obligation/state-desynchronization
testing, with the online guard as a reference repair mechanism. The Rust field
port now includes cross-field effect relations, not merely independent fields;
it still does not infer contracts. Its fused-observation limitation remains.
Compare field and whole-observation judgments with equivalent allowed outcomes,
so a weaker baseline contract does not manufacture a discovery advantage.

Three upstream async-driver patches have now been prepared and reviewed locally;
see [their status](../phase4-exploration/python/pg-mysql/upstream/README.md).
The psycopg async entry patch is separate from this round's synchronous-signal
repair and does not retroactively extend that repair's coverage. Live-owner
claims are implemented in the Rust research guard, with sole-owner authority
for connection-wide recovery; they are not yet a Python ownership integration.
The earlier “next” list is superseded by the [current research decision](../../research/theory/PROGRESS-GUARDS.md#当前收尾判断跨层责任中断与状态失步): prioritize pool handoff and pipeline responsibility,
reuse mature positive controls, and do not add automatic field derivation or a
policy learner merely to expand the framework.
