# Old/new handle comparison (round 5, Work 1)

`successor_probe/probe.py` implements the reusable pair probe:
fresh-nonce echo + agreed column metadata, session diagnostics, open-transaction
boundary check, a unique marker write in a normal transaction, bounded
independent visibility, and a post-close persistence check.  The old handle is
the interrupted/reused handle; the reference is a fresh direct handle.  The
probe compares *handle behaviour*, not the target's intended effect.

## Contract used by `compare()`

- `CLEAN` old and reference: `consistent_clean`.
- `OPEN_TRANSACTION` old: `old_anomaly` (unless the caller explicitly declared
  a legitimate outer transaction, in which case `OPEN_TRANSACTION_ALLOWED` and
  `legitimate_open_transaction`).
- `CLOSED` old: `old_closed`; fail-closed repair is kept separate from
  contamination.
- `BLOCKED_BY_SESSION` / `PROBE_TIMEOUT`: not applicable (`old_not_applicable`),
  never "clean".
- A reference anomaly is `reference_anomaly` (environment/harness problem), not
  evidence about the old handle.
- `echo_mismatch`, `metadata_mismatch`, `protocol_error`,
  `transaction_api_error`, `invisible_write` and
  `write_only_visible_after_settle` are anomalies.  A read-only session,
  statement timeout or permission error is `blocked_by_session`, not
  contamination; "current transaction is aborted" is not treated as a legal
  configuration.

## Results

Local PostgreSQL 18.4, port 32870; tested handle through a 50 ms per-chunk
proxy, reference direct; independent observer connection.  Logs in
`successor_probe/logs/`.

| scenario | target | old | reference | compare | note |
|---|---|---|---|---|---|
| psycopg entry k=1,2 | cancelled | `open_transaction` | clean | **old_anomaly** | server `INTRANS`; target row absent |
| psycopg body k=3,4 | cancelled | clean | clean | consistent_clean | rollback |
| psycopg exit k=5,6 | cancelled | clean | clean | consistent_clean | target row already visible |
| psycopg fix k=1,2 | cancelled | clean | clean | consistent_clean | rollback inside cancellation |
| asyncpg entry k=1 | cancelled | `open_transaction` | clean | **old_anomaly** | server `INTRANS` |
| asyncpg body k=2 | cancelled | clean | clean | consistent_clean | |
| asyncpg exit k=3 | cancelled | clean | clean | consistent_clean | target row visible |
| asyncpg fix k=1 | cancelled | `closed` | clean | **old_closed** | fail-closed termination |
| asyncpg fix k=2,3 | cancelled | clean | clean | consistent_clean | |
| psycopg legit outer k=1 | cancelled | `open_transaction_allowed` | clean | legitimate_open_transaction | explicit allowance |
| psycopg legit nested k=1 | cancelled | `open_transaction_allowed` | clean | legitimate_open_transaction | explicit allowance |
| psycopg session k=3 | cancelled | clean | clean | consistent_clean | retained session settings |
| psycopg read-only k=3 | cancelled | `blocked_by_session` | clean | old_not_applicable | unprobeable, not clean |
| psycopg session/readonly k=1 | cancelled | `open_transaction` | clean | old_anomaly | entry contamination dominates the session state |

The fixed asyncpg handle is reported as `old_closed`, not `old_anomaly`: the
repair is fail-closed and the caller cannot reuse the lease.  The fixed psycopg
handle is clean because it rolls back inside the cancelled `__aenter__`.

## Pool return/reborrow

`run_pool.py` probes the next checkout after returning the interrupted handle.
For psycopg, an entry cut produced a new pid on the next checkout and the old
probe was clean; body/exit cuts reused the same pid cleanly.  For asyncpg, all
tested cuts reused the same pid and probed cleanly.  This is the expected reset
boundary: the pool can repair the handle before a successor borrows it.  It also
means a clean next-checkout probe does **not** prove that a retry inside the
same checkout was safe (see Work 2 and `postfix_lease/`).

## Limits

- The probe does not know whether the target was supposed to commit; it cannot
  distinguish a legitimate commit-after-cancel from a lost write by effect
  alone.  Exit cuts show a visible target row and a clean handle.
- An open transaction is an anomaly only under the declared handoff contract;
  a legitimate outer transaction must be declared by the caller.  Otherwise
  the probe correctly reports an open transaction.
- `blocked_by_session` and probe timeout are unprobeable, not evidence of
  cleanliness.
- The reference is direct while the tested handle goes through the delay proxy;
  a network-path difference is possible but the reference is intended as a
  same-server, fresh-handle baseline, not a byte-identical path.
- The probe writes only in its own schema/table; DDL is outside the tested
  handles.  A pool reset/reborrow can hide a same-checkout silent retry.
