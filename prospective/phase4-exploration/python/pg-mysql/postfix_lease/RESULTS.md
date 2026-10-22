# asyncpg: same-lease reuse after the entry fix (round 5, group 1)

This completes the boundary that round 4 recorded only as a note.  The tested
handle goes through a 50 ms proxy; an independent direct observer confirms that
the server executed `BEGIN` before the cancellation is sent.  The cancellation
is caught without returning or resetting the connection.  Then an ordinary
autocommit `INSERT` is issued on the same connection, the table is read from
the independent observer, and the handle is returned to a one-slot pool for
reset and reborrow.

Original asyncpg 0.32.0 and the local fail-closed patch (terminate the
connection in a cancelled `Transaction.__aenter__`) are run separately.

## Results (2 reps, all 2/2 unless noted)

| mode | original | patched |
|---|---|---|
| `normal` | commit, target visible, next borrow visible | same |
| `entry_same_lease` | same-lease `INSERT` returns `ok`; observer sees **0 rows**; after pool return rows 0; next borrow clean | same-lease call **explicitly refused**; after return 0; next borrow clean |
| `entry_reset` | `conn.reset()` clears the abandoned transaction; same-lease `INSERT` is **visible/committed** | `reset()` refused on the already-released/closed lease |
| `entry_txapi` | `async with conn.transaction()` returns ok, its write is **invisible**; after return 0; next borrow clean | transaction API explicitly refused |
| `body_cancel` | same-lease `INSERT` visible; next borrow clean | same |

The direct-connection control gives the same distinction with a different
message: original returns `INSERT 0 1` but the row is invisible; patched raises
`InterfaceError: connection is closed`.  In the pooled run the patched message
is `InterfaceError: cannot call Connection.execute(): connection has been
released back to the pool` because `asyncpg` detaches the pool proxy when the
underlying connection is terminated.  Both are explicit refusals, not a
successful reuse.

Classification used by `classify()`: `committed_success` (returned and
visible), `explicit_refusal` (raised/closed), `uncommitted_success` (returned
`ok` but invisible), and `after_return_rows_0` (rolled back on pool return).
The original entry case is **uncommitted success**: a caller that trusts the
return value can lose the write.  The patched case is **explicit refusal**: the
caller must not use that lease, and the pool replaces it on the next borrow.

## What this says about the fix

- `_top_xact = None` is not evidence that the server is IDLE.  The observer saw
  `BEGIN` and `idle in transaction` at the cut, while the client-side state was
  already unusable/ownerless.
- The patch protects the **same-lease ordinary-operation** boundary by refusing
  it: it does not make the half-entered transaction usable.  That is a
  legitimate fail-closed contract, not a fake repair.
- The pool return/reborrow boundary is protected even in the original: reset
  rolls the abandoned transaction back, and the next borrow is clean.  This is
  why a next-checkout probe can look clean while a same-checkout retry silently
  loses its write.
- Explicit `reset()` before reuse is a real recovery path in the original
  (`entry_reset`: insert visible), but the patched handle cannot take it
  because it is already closed.
- `entry_txapi` in the original is a second silent-success shape: the nested
  transaction API returns ok, but its write is inside the abandoned transaction.

## Limits

- Two reps per mode, one host, one asyncpg version, experimental monkeypatch
  (no upstream change).  50 ms proxy delay; the BEGIN confirmation is an
  observer `pg_stat_activity` sample, not a packet capture.
- The ordinary INSERT is autocommit; under a different caller transaction the
  result could differ.  The point is the same-lease return value, not a general
  transaction API guarantee.
- `asyncpg` emits a `ConnectionError('unexpected connection_lost() call')`
  future warning after some late cancellations; it is non-fatal and is recorded
  as a limitation, not scored as a defect.
