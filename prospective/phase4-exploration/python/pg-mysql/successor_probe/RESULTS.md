# Old/new handle comparison (round 5, Work 1 — corrected)

> Correction (2026-10-22): the original probe returned on `in_transaction`
> before its write/visibility steps. Entry results therefore measured a state
> check, not an independent black-box effect. The probe now has two separable
> modes: a **state diagnosis** and an **effect probe**. In effect mode the state
> is recorded but does not decide the verdict before the write. The earlier
> asyncpg "fixed" arm was the terminate experiment, not the submitted
> `_top_xact` repair; both actual repairs are tested below.

`successor_probe/probe.py` implements the reusable pair probe. It has a
fresh-nonce echo and column-metadata check, session diagnostics, a transactional
marker write and bounded independent visibility, and a post-close persistence
check. `--state-only` stops after the state/session diagnosis. In effect mode
the probe always attempts the write, even if the handle reports an open
transaction; the state is in `checks`, not in the verdict shortcut.

The old handle is the interrupted/reused handle; the reference is a fresh
direct handle. The probe compares *handle behaviour*, not the target's intended
effect. Each run records the loaded package path/version and the repair marker
(`AsyncTransaction._recover_entry_failure` for psycopg, `Transaction.start`
top-xact clear for asyncpg); `--repair actual` refuses to run if the marker is
absent. Logs:
[`logs/actual-matrix/`](logs/actual-matrix/).

## Contract used by `compare()`

- `CLEAN` old and reference: `consistent_clean`.
- `INVISIBLE_WRITE`: the official transaction call returned success but the
  independent observer never saw the marker. This is an anomaly and is the
  black-box result even when the state diagnosis said `open_transaction`.
- `TRANSACTION_API_ERROR`: the official transaction call was explicitly refused
  (for example, asyncpg's manual-transaction guard). This is an anomaly under
  the write contract, but it is a refusal, not silent success.
- `OPEN_TRANSACTION` (state mode only): `old_anomaly` unless the caller declares
  a legitimate outer transaction.
- `OPEN_TRANSACTION_ALLOWED` (state mode) and `EFFECT_PENDING_OUTER` (effect
  mode): the caller declared a live outer transaction. The effect is not clean
  or dirty yet; it must be re-read after that transaction ends.
- `CLOSED`: `old_closed`; fail-closed repair is kept separate from
  contamination. A closed handle is safe only under a contract that says
  "reconnect"; under a contract that requires reuse it is a failed handoff, not
  a clean one.
- `BLOCKED_BY_SESSION` / `PROBE_TIMEOUT`: not applicable
  (`old_not_applicable`), never "clean".
- A reference anomaly is `reference_anomaly` (environment/harness problem), not
  evidence about the old handle.
- `echo_mismatch`, `metadata_mismatch`, `protocol_error` and
  `write_only_visible_after_settle` are anomalies. A read-only session,
  statement timeout or permission error is `blocked_by_session`, not
  contamination.

## Results (2 reps, all 2/2 unless noted)

PostgreSQL 18.4 on port 32919; tested handle through a 50 ms per-chunk proxy,
reference direct; independent observer. Cuts are the same `CmScenario` target
used by the complete differential.

| scenario | state mode | effect mode | reference | compare |
|---|---|---|---|---|
| psycopg baseline k=1,2 (entry) | `open_transaction` | **`invisible_write`** | clean | **old_anomaly** |
| psycopg actual patch k=1,2 | clean | clean | clean | consistent_clean |
| psycopg baseline/actual k=3,4 (body) | clean | clean | clean | consistent_clean |
| psycopg baseline/actual k=5,6 (exit) | clean | clean | clean | consistent_clean |
| asyncpg baseline k=1 (entry) | `open_transaction` | **`invisible_write`** | clean | **old_anomaly** |
| asyncpg actual patch k=1 | `open_transaction` | **`transaction_api_error`** | clean | **old_anomaly** |
| asyncpg baseline/actual k=2 (body) | clean | clean | clean | consistent_clean |
| asyncpg baseline/actual k=3 (exit) | clean | clean | clean | consistent_clean |

The asyncpg actual-patch row is the important separation. The state diagnosis
still says `open_transaction` (the server transaction is there and
`_top_xact` is `None`); the effect probe then gets an explicit
`InterfaceError: cannot use Connection.transaction() in a manually started
transaction`. It is not an invisible success. State-only evidence alone could
not tell the actual patch from the baseline; the write produced the distinction.

The psycopg actual patch is clean in both modes: its `_recover_entry_failure()`
rolls back the entry and removes the local frame, so the next transaction is a
real top-level transaction and the marker is visible.

## Legitimate outer transaction, nested failure, session and read-only

| scenario | state mode | effect mode | after ending the outer scope |
|---|---|---|---|
| psycopg actual `outer` | `open_transaction_allowed` | `effect_pending_outer` | commit → marker **visible** (2/2) |
| psycopg actual `nested` | `open_transaction_allowed` | `effect_pending_outer` | inner+outer rollback → marker **absent** (2/2) |
| psycopg actual `session` | clean | clean | session settings retained; probe clean (2/2) |
| psycopg actual `readonly` | clean | `blocked_by_session` | unprobeable, **not** dirty (2/2) |

This validates the pending state rather than treating every open transaction as
contamination. It also keeps the read-only case "not applicable": the write
cannot run, but that is a legitimate session contract, not handle damage.

## Pool return/reborrow

`run_pool.py` probes the next checkout after returning the interrupted handle.
Baseline and actual patch were both tested (2 reps per cut).

| driver / cut | next checkout | old probe | reference | compare |
|---|---|---|---|---|
| psycopg baseline k=1 (entry) | new pid 2/2 | clean | clean | consistent_clean |
| psycopg baseline k=2 (body) | same pid 2/2 | clean | clean | consistent_clean |
| psycopg actual k=1,2 | same pid 2/2 each | clean | clean | consistent_clean |
| asyncpg baseline/actual k=1 | same pid 2/2 each | clean | clean | consistent_clean |

The pool's reset is the repair here. A clean next-checkout probe does **not**
prove that a retry inside the same checkout was safe; `postfix_lease/` shows
the same-lease ordinary INSERT still returning `ok` with no visible row under
the actual asyncpg patch.

## What the reference adds

The fresh reference is clean in every row above, including the contaminated
baseline entry trials and the actual asyncpg refusal trial. It is a calibration
control: it shows the anomaly is a property of the interrupted/reused handle and
not of the server, schema, observer or probe itself. It did **not** add a new
defect discovery on these targets; the complete differential already found the
psycopg entry violation and the asyncpg baseline/actual boundaries. Its
increment is classification (invisible vs refused vs pending vs unprobeable)
and a check that the probe's own write works on a fresh handle.

## Limits

- The probe does not know whether the target was supposed to commit; it cannot
  distinguish a legitimate commit-after-cancel from a lost write by effect
  alone. Exit cuts show a visible target row and a clean handle.
- An open transaction is an anomaly only under the declared handoff contract;
  a legitimate outer transaction must be declared by the caller.
- `blocked_by_session` and probe timeout are unprobeable, not evidence of
  cleanliness.
- The reference is direct while the tested handle goes through the delay proxy;
  it is a same-server fresh-handle baseline, not a byte-identical path.
- A cut index does not pin the wire phase. In the asyncpg actual-patch runs,
  `k=1` sometimes cancelled before `BEGIN` reached the server (clean) and
  sometimes after it (explicit transaction-API refusal). The targeted
  observer-confirmed-BEGIN lease matrix resolves that timing distinction; the
  probe records whichever state it actually sees.
