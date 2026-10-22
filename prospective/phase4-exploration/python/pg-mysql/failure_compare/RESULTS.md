# Ordinary failure vs one-sided interruption at matched transaction phases

Group 4, `prospective/phase4-exploration/python/pg-mysql/failure_compare/`.
New Python experiments plus explicitly labelled reuse of the Rust future-drop
case and the offline-discovery sync-signal runs.

## 1. Question

At three semantic positions of a transaction block — **entry** (`BEGIN` /
`__aenter__`), **body**, **exit-commit** (`COMMIT` / `__aexit__`) — what does a
real one-sided interruption do, compared with an ordinary failure at the same
phase, with normal completion as the control?

For each run the records keep separate: client registration/state, server state,
who holds responsibility afterwards, what the successor returns, what an
independent connection can see, and what cleanup is still owed. No single
verdict is forced; legitimate differences and unrealised cases are kept.

**Bottom line (details and counts below).**

- Body: real cancellation and ordinary `1/0`/`23505` both roll back cleanly;
  no cancellation-specific difference was measured.
- Entry: cancellation leaves a **healthy** transaction with no active block
  owner; both drivers' next CM returns normally while its write is never
  committed (3/3 each). Ordinary entry failure is loud (`25P02`) or leaves a
  live owner; psycopg's counter/cleanup dead-end is shared with that ordinary
  failure, asyncpg's ownerless `_top_xact` is specific to cancellation.
- Exit: cancellation can leave the target **committed** while the caller only
  sees `CancelledError` (3/3 both); ordinary deferred-`23505` failure is loud
  and rolls back. Verify after a cancelled commit; do not assume rollback.
- Reuse: sync `entry_wait` and sqlx `BEGIN` future-drop agree in shape with
  entry cancellation; sync pre-send/exit-pre-commit windows and a
  commit-reply-only Rust arm remain unrealised/Unknown, not forced into the
  matrix.

## 2. Method

- Real asyncio `Task.cancel()` through `cases/txphase_case._PhasedTask` /
  `_await_phased`; no synthetic `CancelledError` and no injected
  `ErrorResponse`.
- Real server failures: `SELECT 1/0` (`22012`), immediate unique violation
  (`23505`), deferred unique violation raised by `COMMIT` (`23505`), failed
  `SAVEPOINT` into an already-aborted transaction (`25P02`), and
  `pg_terminate_backend` (`57P01` / closed connection).
- Tested handle goes through the unmodified retro `DelayProxy` (50 ms per
  server→client chunk); the observer is a separate direct connection. Every
  trial has its own table
  (`k text primary key, v int not null, u int unique deferrable initially deferred`)
  and drops it afterwards.
- PostgreSQL 18.4 (Alpine, x86_64), database `dw`, `127.0.0.1:32870`.
  Python 3.13.13, psycopg 3.3.6, asyncpg 0.32.0, `autocommit=True` for the
  tested handle. `psycopg_pool` is installed but unused; no pool in this matrix.
- All operations bounded (`BOUND=2.0 s`, successor `10 s`); a distinct table
  per trial; nothing is run against a production database.

### What is new, what is reused

| mechanism | provenance | what it covers |
|---|---|---|
| psycopg 3.3.6 `Task.cancel` + real failures | **new**, 30 trials | entry/body/exit; real server errors; normal control |
| asyncpg 0.32.0 `Task.cancel` + real failures | **new**, 30 trials | entry/body/exit; real server errors; normal control |
| sqlx 0.9.0 future drop | **reused, newly executed**: existing compiled `rescan` binaries, 3 reps release + 3 reps PR #4394 | `Pool::begin` round-trip (entry), whole transaction, statement-level controls |
| sync psycopg signal | **reused, not rerun**: offline-discovery phase matrix + real-signal sweep | `KeyboardInterrupt` at sync entry/body/exit positions |
| sqlx round-4 statement ladder | **reused**: 48 cut trials, 12 baselines | prepare/execute statement boundaries only; no transaction |

The reused mechanisms are not treated as equivalent to `Task.cancel`: the Rust
case drops futures and sends no protocol `CancelRequest`; the sync case raises
`KeyboardInterrupt` in the calling thread and never enters asyncio.

### Commands

```sh
cd prospective/phase4-exploration/python/pg-mysql
./failure_compare/run.sh                      # both drivers, 3 reps, live :32870
./failure_compare/run-futuredrop.sh           # existing sqlx binaries, 3+3 reps
python failure_compare/analyze.py failure_compare/logs/run.fc.20261022-143304
```

`run.sh` sets `PYTHONPATH=".:$REPO/research/python-retro"` and uses
`/home/ava/dropwise-py-work-r3/venv313/bin/python` by default (override with
`DW_PYTHON`, `DW_PG_PORT`, `REPS`).

### Cost

- Main matrix: 60 trials (30/driver) + 11 calibration scenarios; **112 s wall**
  (`14:33:04`–`14:34:56`), 95.3 s summed trial time, 1.04–1.90 s/trial.
- Future-drop reuse: 3 release + 3 fixed runs, ~7.3 s each (~45 s total).
- No container was started for the main run; one host, one loopback server.

## 3. Phase calibration

Cuts are chosen by semantic phase, not by equal `k`. `k` counts yields of the
target task; calibration maps each `k` to a phase and all selected cuts were
realised 3/3.

| driver | yields | entry | body | exit-commit | cuts used |
|---|---:|---|---|---|---|
| psycopg | 6 | 1–2 | 3–4 | 5–6 | k=1, 3, 5 |
| asyncpg | 3 | 1 | 2 | 3 | k=1, 2, 3 |

The phase is recorded at `Task.cancel()` (`holder["phase"]`); the exact await
at which the cancellation is delivered can vary within the calibrated range, and
server/client state was measured after each cut. `realized` means the target
ended `cancelled`/`error` after the cut; unrealised trials would be reported,
not interpreted (0 here).

## 4. New Python matrix

All cells are 3/3 identical. Abbreviations: `IDLE/0` = psycopg
`tx_status=IDLE`, `num_transactions=0`; `INTRANS/1`, `INERROR/1`, `UNK/1` (UNKNOWN)
likewise; `tx ...` is the captured transaction object (for the
nested-savepoint arms, the outer CM), not a claim about the failed nested entry;
`in_tx T/F` = asyncpg `in_transaction()`; `S` = successor returned
normally but its own write was **not** independently visible (and recovery
rolled it back); `ok+` = successor returned and its write was visible;
`V` = target row visible; `−` = target row not visible; `reconn` = fresh
connection; P/A = psycopg/asyncpg. The successor is a fresh
`conn.transaction()` on the same handle after the arm (after the outer exit in
the savepoint arms, after reconnect for the terminated arm); the target is the
tested transaction's own write. Each
cell also names the observed error.

| arm | psycopg state / server | psycopg successor / target | asyncpg state / server | asyncpg successor / target | holder |
|---|---|---|---|---|---|
| `normal` | IDLE/0, tx COMMITTED / idle | — / **V** | in_tx F, tx COMMITTED / idle | — / **V** | none (committed) |
| `cancel_enter` | **INTRANS/1** / idle in tx (BEGIN) | **ok S** / − | **in_tx T, tx+top FAILED** / idle in tx (BEGIN;) | **ok S** / − | orphan transaction object; no block |
| `cancel_body` | IDLE/0 / idle (ROLLBACK) | ok+ / − | ROLLEDBACK / idle | ok+ / − | block; exits rollback |
| `cancel_exit_commit` | IDLE/0, tx COMMITTED / idle (COMMIT) | ok+ / **V** | in_tx F, tx **FAILED** / idle (COMMIT;) | ok+ / **V** | driver; commit already applied |
| `server_body_div` | IDLE/0 / idle (ROLLBACK) | ok+ / − | ROLLEDBACK / idle | ok+ / − | block; exits rollback |
| `server_body_unique` | IDLE/0 / idle (ROLLBACK) | ok+ / − | ROLLEDBACK / idle | ok+ / − | block; exits rollback |
| `server_body_swallowed_commit` | IDLE/0, tx COMMITTED / idle (COMMIT) | ok+ / − | in_tx F, tx COMMITTED / idle | ok+ / − | caller holds a false success |
| `server_exit_commit_deferred` | IDLE/0, tx **COMMITTED** / idle (COMMIT) | ok+ / − | in_tx F, tx FAILED / idle (COMMIT;) | ok+ / − | caller holds `23505` |
| `server_entry_savepoint` | **INERROR/1**, tx COMMITTED / idle in tx aborted (SAVEPOINT) | **fail 25P02** / − | in_tx F, tx **COMMITTED** / idle (COMMIT;) | ok+ / − | P: broken outer CM; A: live outer |
| `server_entry_terminated` | UNK/1, **closed** / absent | reconn ok+ / − | **closed** / absent | reconn ok+ / − | none; connection gone |

Error classes: `cancel_*` all `CancelledError` (psycopg exit target reports
`cancelled`); `div` `DivisionByZero`/`DivisionByZeroError` `22012`; `unique`
and deferred `UniqueViolation`/`UniqueViolationError` `23505`; savepoint
`InFailedSqlTransaction`/`InFailedSqlTransactionError` `25P02`; terminated
`AdminShutdown 57P01` (psycopg) / `InterfaceError: connection is closed`
(asyncpg).

### Cleanup recorded per arm (`cleanup_obligation` in the JSONL)

| arm | psycopg | asyncpg |
|---|---|---|
| `cancel_enter` | `rollback()` rejected by guard (`ProgrammingError`); raw `ROLLBACK` succeeds but `_num_transactions` stays 1; next CM raises bare `AssertionError`; close/reconnect | `reset()` clears it; without reset a fresh CM silently nests inside the abandoned `_top_xact` |
| `cancel_body`, `server_body_div/unique` | none; connection reusable | none; reusable |
| `cancel_exit_commit` | none; reusable; independently verify the commit | none; reusable; verify independently |
| `server_body_swallowed_commit` | none; caller must not trust the reported success | same |
| `server_exit_commit_deferred` | none; `23505` already delivered | none |
| `server_entry_savepoint` | outer exit raises `OutOfOrderTransactionNesting` (counter 1); the failed successor entry raises it to 2; raw rollback leaves 2; next CM `AssertionError`; close | live outer exit clears the state; reusable |
| `server_entry_terminated` | reconnect | reconnect |

The full per-trial values (server `pg_stat_activity` state/query, pid, client
fields, successor stages, recovery results) are in `trials.jsonl`; the
aggregate is in `summary.json` / `summary.md`.

## 5. Findings

### 5.1 Body: interruption and ordinary failure coincide

- `cancel_body`, `server_body_div`, `server_body_unique`: all 3/3 clean
  rollback, connection `IDLE`, target invisible, successor write visible.
- No cancellation-specific body hazard was observed. The CM's exit path with
  either an exception or a cancellation sends rollback here.
- A different ordinary-failure hazard appears when the body error is
  **swallowed**: catch `1/0`, write, then `COMMIT`. Both drivers report
  success/`COMMITTED`, but the target is invisible 3/3 (target silent success). PostgreSQL returns a `ROLLBACK` tag for `COMMIT` of an aborted
  transaction; both drivers surface this as a committed transaction. This is
  **common to ordinary failure**, not cancellation-specific.

### 5.2 Entry: the one-sided case produces a silent successor

`cancel_enter` lands with the server already in a healthy `INTRANS`
transaction; the client received no PostgreSQL error and no block owns the
transaction afterwards.

- psycopg: `_num_transactions=1`, server `idle in transaction`. A fresh
  `conn.transaction()` nests as `SAVEPOINT "_pg3_2"`, returns normally
  (`RELEASE`), and its insert is invisible to the observer (`ok S`, 3/3).
  Recovery dead-ends: `conn.rollback()` → `ProgrammingError: Explicit
  rollback() forbidden within a Transaction context`; raw `ROLLBACK` clears
  the server but not the counter; the next CM raises `AssertionError`. Final
  rows `[]`.
- asyncpg: `_top_xact` still points at the cancelled transaction object with
  state `FAILED`, server `idle in transaction`. A fresh CM sees `_top_xact` and
  nests as a savepoint; it returns normally and its write is invisible
  (`ok S`, 3/3). `conn.reset()` is the working cleanup; without it a fresh CM
  silently nests.
- Ordinary entry failure is **loud or owned**, not silent:
  - psycopg `server_entry_savepoint` leaves `INERROR/1`; the successor fails
    loudly with `25P02` (3/3). But the counter leak and cleanup dead-end are
    similar to `cancel_enter`: `OutOfOrderTransactionNesting` on outer exit,
    rollback guard, raw rollback, `AssertionError` on retry. The bookkeeping
    trap is **common to the ordinary failed-entry arm**; only the healthy
    `INTRANS` + silent successor is cancellation-specific.
  - asyncpg `server_entry_savepoint` leaves the live outer block owning
    `_top_xact`; the outer exit clears it and the successor is visible 3/3.
    Here ordinary failure has a responsible owner; `cancel_enter` has none.
  - `server_entry_terminated` is a closed connection, loudly failed and
    recoverable by reconnect; no silent successor.

### 5.3 Exit: the commit can be applied while the caller sees cancellation

- `cancel_exit_commit` (3/3 both): target committed and independently visible;
  successor write also visible. The caller only saw `CancelledError`/cancelled.
  - psycopg local transaction says `COMMITTED`.
  - asyncpg local transaction says `FAILED` even though the server committed.
  Both connections are `IDLE` and reusable. The one-sided obligation is
  epistemic: the caller cannot tell from the cancellation whether `COMMIT`
  applied. This sample committed each time, but that is not a guarantee.
- Ordinary exit failure (`server_exit_commit_deferred`) is loud `23505` and the
  target is invisible 3/3. psycopg still records `tx_status=COMMITTED`
  locally, despite the server rollback; asyncpg records `FAILED`, which matches
  the rollback. The local status is not a reliable witness in either direction.
- `normal` commits and is visible 3/3.

### 5.4 Common vs cancellation/one-sided-specific

**Observed on ordinary failure too (not cancellation-specific):**

1. `COMMIT` of an aborted transaction is reported as success by both drivers
   (`server_body_swallowed_commit`, 3/3; also the asyncpg outer exit after a
   handled nested `25P02` reports `ok`/`COMMITTED` with the target invisible).
2. psycopg outer `__aexit__` after a handled body error and a failed nested
   entry raises `OutOfOrderTransactionNesting` and leaves `_num_transactions`
   nonzero; rollback guard, raw rollback, and next CM `AssertionError` follow
   (`server_entry_savepoint`, 3/3).
3. psycopg `COMMIT` failure leaves local `tx_status=COMMITTED` while the
   server rolled back (`server_exit_commit_deferred`, 3/3).

**Specific to cancellation / one-sided exit in this matrix:**

1. Entry cancellation is the only case where a healthy connection has **no
   error and no transaction owner**, and a fresh CM's successor returns
   normally while its write is never committed (`cancel_enter`, 3/3 both
   drivers). The psycopg counter leak itself is shared, but ordinary failed
   entry is `INERROR`/loud; asyncpg ordinary failed entry has a live outer
   owner.
2. Exit cancellation can commit while the caller sees only cancellation
   (`cancel_exit_commit`, 3/3 both). Ordinary deferred failure is loud and
   rolled back; asyncpg's local `FAILED` at a successful commit is
   cancellation-specific in this sample.

**Legitimate differences, not forced into one verdict:**

- psycopg and asyncpg expose different local statuses after exit cancellation
  (`COMMITTED` vs `FAILED`) while the observer sees the same commit. Neither
  public contract promises a status after cancellation; the comparison reports
  the divergence rather than choosing a winner.
- The ordinary entry arms are structurally different (psycopg nested
  `SAVEPOINT` inside a managed outer; asyncpg nested CM inside a live outer),
  so their cleanup differs. Within each driver the cancellation/ordinary
  contrast is the meaningful one.
- Synchronous pre-send states and Rust future drops are different mechanisms;
  only shape/phase comparisons are made below.

## 6. Reused Rust future drop (newly executed, existing code)

Source: `prospective/t3-sqlx/tests/rescan.rs`, report
`prospective/RESCAN.md`, fix `prospective/t3-sqlx/patches/sqlx-pr4394-transaction.diff`.
The already-compiled binaries were executed; nothing was recompiled or edited:

- release: `.../t3-sqlx/target/debug/deps/rescan-252ffa1474962797`
  (sha256 `888e4a28…e803f`)
- fixed: `~/dropwise-rescan-work/t3-sqlx-pr4394/.../rescan-6b5818a3ef9f61d7`
  (sha256 `4e247513…e744d8`)

3 release + 3 fixed repetitions, all 3/3 identical. Fresh results reproduce
`RESCAN.md` exactly:

| case | release plans / violations | fixed plans / violations | what the violation is |
|---|---|---|---|
| R3.1 `Pool::begin` | 4 / **2** (Post 2, Neither 2) | 4 / **0** (Post 4) | 2 plans at the `BEGIN` round-trip; successor's autocommit write invisible (0 rows, should be 1) |
| R3.5 whole transaction | 8 / **2** (Pre 4, Post 2, Neither 2) | 8 / **0** (Pre 6, Post 2) | 2 Neither inside `begin()`; the other 6 are clean Pre/Post |
| R3.6 two autocommit writes | 10 / **6** (Pre 2, Post 2, Neither 6) | 10 / **6** | negative control: first statement committed, second not; **not** a transaction-body case |
| R3.2 `acquire` | 2 / 0 | 2 / 0 | clean |
| R3.3 `prepare` | 2 / 0 | 2 / 0 | clean |
| R3.4 old `fetch_one` target | 4 / 0 | 4 / 0 | clean |

The `echo` observation (the frozen study's `SELECT $1` check) reports **0
violations** for R3.1 and R3.5 in both variants: the old observation misses the
invisible write. This is the same lesson as the Python matrix: successor
success is not independent visibility.

The Rust event is a **future drop**, not `Task.cancel`; it sends no protocol
cancel request. The reported R3.1 mechanism nevertheless matches entry cancellation:
the pooled connection is left in a transaction and the successor writes inside
it. The upstream fix claims the depth before the `BEGIN` round-trip, so the
cancelled begin has a queued rollback; fixed R3.1/R3.5 have no violations. The case has
**no separate commit-reply phase**; R3.5 is a whole-transaction cut and cannot
be used as an exit-only measurement.

The older statement-level case (`clients/sqlx/logs/run.Eycpui/`,
`RESULTS-FRAGMENT.md`) has **48 cut trials / 12 baselines, 0 violations**, but
it is autocommit statement-level (`prepare`/execute), explicitly “no commit,
no transaction”; it is not counted as phase-matched evidence.
`prospective/t3-sqlx/tests/commit_reply.rs` exists but is a manual `select!`
application-policy test with no saved run/logs; it is left **unmeasured /
Unknown**, not added to the matrix.

## 7. Reused sync signal (not rerun)

Source: `prospective/offline-discovery/logs/2026-10-22/SUMMARY.md`,
`phases2.jsonl` (72 phase trials), `docs/RESULTS-sync-signal.md`,
`ROUND5-RESULTS.md`.

Phase matrix (6 phases × 4 modes × 3 reps; every cell 3/3 identical):

| phase | verdict (retry/survivor) | state after interrupt |
|---|---|---|
| body | post 3/3 | clean |
| entry_wait | **silent-loss 3/3** | `wait_intrans` |
| entry_pre_send | **explicit-failure 3/3** | `pre_send_local_ahead` (deterministic injection) |
| entry_after_send | **explicit-failure 3/3** | `unread_reply` (deterministic injection) |
| exit_pre_commit | **silent-loss 3/3** | `intrans_no_local_owner` (deterministic injection) |
| exit_wait | post 3/3 | clean |

Recovery for `entry_wait` / `entry_pre_send`: `rollback()` forbidden 0/3, raw
`ROLLBACK` ok 3/3, reuse `transaction()` 0/3 — the same psycopg dead-end measured
asynchronously in `cancel_enter` (for `entry_wait`). `entry_after_send` is worse:
rollback, raw rollback, reuse and plain insert all fail 3/3 (unread reply).
The `exit_pre_commit` connection recovers (`rollback()` and reuse both ok), but
its successor / retry are still silent-loss (the target was never committed). `entry_wait` is the sync analogue of psycopg async
entry; `exit_wait` matches async exit cancellation (commit applied, Post);
`body` matches async body (clean).

Provenance is mixed and explicitly labelled in the source report: `body`,
`entry_wait`, `exit_wait` were reached by a real `SIGALRM`/`SIGINT`-style
signal; `entry_pre_send`, `entry_after_send`, `exit_pre_commit` are
deterministic line injection (synthetic landing, real psycopg protocol state).
The 60-real-signal sweep (48 calibrations; 22 clean / 8 pre-send / 12 unread
reply / 18 wait) shows real reachability of the pre-send and unread-reply
states; `exit_pre_commit` is a sync-only deterministic state. The sweep has
**no ordinary-failure control**. This arm is reused only for the interruption side;
no new sync runs were made.

## 8. Cross-mechanism phase comparison

| semantic phase | new `Task.cancel` | new real server failure | reused sqlx future drop | reused sync signal |
|---|---|---|---|---|
| entry | psycopg: `INTRANS/1`, successor ok **S**, dead-end cleanup; asyncpg: ownerless `_top_xact` FAILED, successor ok **S** | psycopg `25P02` INERROR, loud successor, same counter dead-end; asyncpg live outer clears; terminated closed/reconnect | R3.1 `BEGIN` pending: 2/4 plans successor invisible (release); **0/4 fixed**; echo misses it | `entry_wait` silent-loss 3/3 (matches psycopg). `entry_pre_send` / `entry_after_send` are explicit-failure and **not realised** by async `Task.cancel` (no pre-send await exists; the cancellation cleanup also consumes the pending reply) |
| body | clean rollback; successor visible 3/3 both | `1/0`, `23505`: clean, successor visible; swallowed error + `COMMIT`: false success 3/3 | R3.6 two autocommit writes: 6/10 half-complete (negative control, not a transaction body); R3.2–R3.4 statement-level clean | body clean/post 3/3 |
| exit-commit | target committed/visible 3/3 both; caller sees `CancelledError`; psycopg COMMITTED, asyncpg FAILED | deferred `23505`: loud, target invisible; psycopg local COMMITTED | R3.5 whole transaction: 2/8 Neither inside `begin()` (release), **0/8 fixed**; no commit-reply-only arm | `exit_wait` post 3/3 (matches); `exit_pre_commit` silent-loss 3/3 but is **pre-send**, no async analogue |

Unrealised distinctions preserved: async `Task.cancel` at an `await` cannot
land before the command is sent, so sync `entry_pre_send` and
`exit_pre_commit` have no corresponding async cell; async `Task.cancel` cleanup
leaves no unread reply in this sample, so sync `entry_after_send`’s
`unread_reply` is not reproduced. These are marked unrealised, not counted as differences in
defect rates. Conversely, the Rust case has no commit-reply-only measurement.

## 9. Limitations / Unknowns

- 3 repetitions per cell, one host, one PostgreSQL 18.4 server, one driver
  version each; phase windows are existence/consequence measurements, not
  production probabilities.
- One selected `k` per semantic phase; calibration shows the phase ranges, but landing
  lines can move with scheduling and the 50 ms proxy. The proxy delays only
  server→client chunks and only on the tested connection; a real network may
  change the windows.
- Ordinary entry failure cannot be a plain refused `BEGIN` in these drivers
  without killing/configuring the server; the ordinary entry arms are a real
  failed nested `SAVEPOINT` (`25P02`) and a terminated backend. They are real
  server failures but structurally different from “BEGIN itself refuses”.
- asyncpg state is read from private fields (`_top_xact`, `_state`);
  `Transaction.__aenter__()` returns `None` in 0.32.0. The observations are
  version-specific and not a public API contract.
- `pg_terminate_backend` is a server-side termination, not a cancellation.
- Observer sees committed rows only, at default isolation; a row invisible to
  the observer could in principle be visible in another session/isolation.
- The swallowed-error arm intentionally catches an error and commits; it is a
  control for “client says success”, not a recommended usage.
- Reused Rust binaries were not recompiled; only RESCAN summary lines are
  saved, not per-plan JSONL. Reused sync data is from 2026-10-22 and mixes real
  signal with deterministic injection; no ordinary-failure control exists
  there.
- No pool, no MySQL, no production application; this is a single-handle
  transaction-phase matrix.

## 10. Files

New, all under `failure_compare/`:

- `compare.py` — matrix, calibration, real cancellations/failures, JSONL.
- `precheck.py` — exploratory direct-connection probe used to choose arms.
- `analyze.py` — aggregates `trials.jsonl` into `summary.json` / `summary.md`.
- `run.sh`, `run-futuredrop.sh` — authoritative runners.
- `logs/run.fc.20261022-143304/` — `trials.jsonl`, `calibration.jsonl`,
  `calibration-{psycopg,asyncpg}.json`, `meta.json`, `run.log`, `summary.*`.
- `logs/futuredrop-reuse.20261022-143127/` — 3 release + 3 fixed logs,
  `summary.txt`, `environment.txt` (binaries and sha256).
- `logs/precheck.txt` — precheck output.

Reused, unchanged: `prospective/t3-sqlx/tests/rescan.rs`, its compiled
binaries, `prospective/RESCAN.md`, `clients/sqlx/logs/run.Eycpui/`,
`prospective/offline-discovery/logs/2026-10-22/`,
`prospective/offline-discovery/docs/RESULTS-sync-signal.md`.

No tracked file was modified; no upstream contact was made.
