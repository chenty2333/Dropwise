# asyncpg: same-lease reuse after the entry fix (round 5, group 1 — corrected)

> Correction (2026-10-22): the earlier "patched" arm was the experimental
> terminate-on-entry monkeypatch, not the submitted `_top_xact` repair. The
> actual-patch matrix is below. It changes the conclusion: the actual patch
> does **not** refuse a same-lease ordinary operation; it only stops the stale
> registration from turning the next transaction into a savepoint. The
> experimental arm's explicit refusal is retained as a separate, narrower
> strategy result.

The tested handle goes through a 50 ms proxy. An independent direct observer
confirms that the server executed `BEGIN` before cancellation. The
cancellation is caught without returning or resetting the connection. Then an
ordinary autocommit `INSERT` is issued on the same connection, the table is
read from the independent observer, and the handle is returned to a one-slot
pool for reset and reborrow.

Variants are selected by the loaded package, not by a flag name:
`original` = asyncpg 0.32.0; `actual` = `/home/ava/Worktrees/asyncpg/repo`
(HEAD `ff1bb8d`, `Transaction.start()` identity-guard source present);
`experimental` = 0.32.0 plus `install_asyncpg_enter_fix()`. Every record carries
`package.file`, version, `start_has_top_xact_clear`, and whether
`Transaction.__aenter__` contains `terminate()`. Logs:
[`logs/actual-matrix/`](logs/actual-matrix/).

## Results (3 reps per pool mode, all 3/3 unless noted)

| mode | original | actual `_top_xact` patch | experimental terminate |
|---|---|---|---|
| `normal` | commit/visible; next borrow visible | same | same |
| `entry_same_lease` | `INSERT 0 1`; observer **0 rows**; after return 0 | **`INSERT 0 1`; observer 0 rows; after return 0** | explicit refusal (pool proxy detached/closed); after return 0 |
| `entry_reset` | reset then INSERT **visible** | **reset then INSERT visible**; after return 1 | reset refused (already released) |
| `entry_txapi` | nested API returns ok, write **invisible** | **`InterfaceError: cannot use Connection.transaction() in a manually started transaction`** | transaction API refused (pool message) |
| `body_cancel` | same-lease INSERT visible | same | same |

All next borrows are clean in these cells. The actual patch's same-lease
ordinary INSERT is therefore still an **uncommitted success** in this sample;
the old experimental arm's `explicit_refusal` is not a property of the
submitted patch. The actual patch's one clear same-lease protection is the
transaction API: `_top_xact` is `None`, but `_protocol.is_in_transaction()` is
true, so `Transaction.start()` refuses instead of silently using a savepoint.

## Legitimate outer transaction and nested failure

`outer_nested` starts an outer transaction, writes `outer_before`, provokes an
ordinary nested-start failure (different isolation), then cancels a second
nested start, writes `outer_after`, and commits the outer transaction.
`outer_control` does the same without a nested failure.

| variant | outer commit visible | outer kept after ordinary nested | outer kept after cancelled nested | result |
|---|---:|---:|---:|---|
| original | 2/2 | 2/2 | 2/2 | outer commits, both rows visible |
| actual | 2/2 | 2/2 | 2/2 | outer commits, both rows visible |
| experimental | 2/2 on control | 2/2 | **0/2** | cancelled nested start terminates the whole connection; outer commit fails |

The actual patch's `if con._top_xact is self` identity guard preserves the
outer registration when a nested start fails. The experimental terminate
policy is too broad: it kills a legitimate outer transaction on a nested
cancellation. The actual patch is narrower and safer on this boundary.

## What changed in the interpretation

- The actual patch does **not** make the half-entered transaction usable. It
  clears a stale local registration so the next transaction is not silently
  nested. A caller that continues on the same lease with an ordinary
  autocommit statement can still get `ok` and lose the write.
- The safe same-lease continuations observed here are: explicitly
  `reset()`/rollback/close before reuse (reset works with the actual patch), or
  use a pool return/reborrow, whose reset rolls the abandoned transaction back.
- The transaction API is an explicit refusal boundary with the actual patch,
  because the protocol still knows it is in a transaction. That is a guard,
  not transparent recovery.
- `_top_xact = None` alone is not evidence of IDLE; the observer still sees
  `BEGIN`/`idle in transaction` at the cut.
- The pool return/reborrow boundary is clean in original and actual. A
  next-checkout probe can look clean while a same-checkout retry silently
  loses its write; these are different handoffs.

## Limits

- Two or three reps per cell, one host, one asyncpg version and one delay.
  `BEGIN` confirmation is a `pg_stat_activity` sample, not a packet capture.
- The ordinary INSERT is autocommit; a different caller transaction may
  differ. The claim is about the return value on the same lease.
- The experimental arm is retained only as a strategy comparison. Its results
  must not be attributed to the submitted patch.
- asyncpg can emit a non-fatal `unexpected connection_lost()` warning after
  late cancellations; it is recorded, not scored as a defect.

## MySQL actual patch focused re-check

The MySQL round-5 delay matrix used `fix/patched` (old experimental
`_close_reason`/`InterfaceError`), not the actual `e7a02b6`
(`_is_poisoned`/`_abort()`/`OperationalError`). A focused re-check of the actual
worktree (`/home/ava/Worktrees/mysql-connector/aio-cancel`, 2 reps per mode) is
in [`logs/actual-matrix/mysql-actual.jsonl`](logs/actual-matrix/mysql-actual.jsonl)
and `mysql-original.jsonl`.

| mode | original 26.7.0 | actual `e7a02b6` |
|---|---|---|
| normal | commit/visible (`[["target", 1]]`) | same |
| entry | rollback/reset return `ok`; successor `ProgrammingError: ... 'NoneType' object is not subscriptable`; socket open | `poisoned=True`, `socket_open=False`; rollback/reset/successor all `OperationalError: MySQL Connection not available`; observer 0 rows |
| nested | rollback/reset `ok`; successor `ok` with `rowcount=0`; outer row invisible | poisoned/closed; successor refused; observer 0 rows |
| post-commit | same silent `ok/0` path; cancellation may precede the commit on the wire | poisoned/closed; successor refused; observer 0 rows |

The actual patch's contract is explicit refusal/poisoning, not transparent
recovery: the caller must acquire a new connection. The original's `ok/0`
successor is the dangerous shape: a return value that looks successful while the
outer effect is absent.
