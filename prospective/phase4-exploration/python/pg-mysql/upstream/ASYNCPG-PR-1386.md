# Fix stale `_top_xact` after a failed `Transaction.start()`

Fixes #1386.

## Root cause

`Transaction.start()` registers itself as `Connection._top_xact` before it
sends `BEGIN`.  If the `BEGIN` command raises, `_state` is set to `FAILED`,
but `_top_xact` is left pointing at the failed transaction.  Every later
`Connection.transaction()` then takes the nested path and sends `SAVEPOINT`
instead of `BEGIN`.

This has two user-visible consequences:

* If `BEGIN` never reached the server (for example, the connection was busy
  with another command), later transactions fail with
  `NoActiveSQLTransactionError` until the connection is reset or closed.
* If the task is cancelled after the server has already executed `BEGIN`,
  the next transaction creates a savepoint inside the abandoned server-side
  transaction and reports success, but only `RELEASE SAVEPOINT` is sent at
  the end.  The outer transaction is never committed, so the write is
  invisible to other connections.

## Fix

Only clear `_top_xact` when the transaction that failed to start is still the
top-level transaction:

```python
except BaseException:
    self._state = TransactionState.FAILED
    if con._top_xact is self:
        con._top_xact = None
    raise
```

The identity check is what separates a failed top-level `BEGIN` from a failed
nested `SAVEPOINT`: a nested block must leave the caller's outer transaction
in place.  Error types and cancellation semantics are unchanged; the guard
only removes the stale registration.

This follows the guard suggested in the issue report.

## What happens in each case

* **BEGIN failed before being sent** (connection busy): there is no server
  transaction, `_top_xact` is cleared, and the next transaction issues a
  normal `BEGIN` and commits its writes.

* **Task cancelled after the server executed BEGIN**: reuse either
  commits visibly or is explicitly refused, depending on how much of the
  cancellation has been processed.  If the connection is reused before the
  cancellation is acknowledged, the next transaction sends `BEGIN` inside
  the still-open server transaction; its `COMMIT` commits that transaction,
  so the write is visible.  Once the cancellation has been processed,
  `is_in_transaction()` is true and `Transaction.start()` raises the
  existing `InterfaceError` ("cannot use Connection.transaction() in a
  manually started transaction").  `reset()` then rolls the transaction back
  and reports the existing "Resetting connection with an active transaction"
  warning, after which the connection is reusable.

* **Nested block failed**: `_top_xact` is still the outer transaction, so
  the guard does nothing.  The outer transaction stays current, its
  commit/rollback still works, and a new transaction can be started after it
  exits.  This covers both an isolation-level mismatch (raised before the
  savepoint is sent) and a server-side `SAVEPOINT` error.

* **Pool release**: releasing the connection calls `Connection.reset()`,
  which rolls back the abandoned transaction and emits the same
  "Resetting connection with an active transaction" warning because
  `_top_xact` is `None` while the server is still in a transaction.  The
  next connection acquired from the pool is clean.

## Tests

Six regression tests in `tests/test_transaction.py`, all using independent
observer connections:

* pre-send `BEGIN` failure caused by a busy connection;
* the issue's cancellation repro (cancel scheduled with `loop.call_soon`);
* cancelled `BEGIN` after the cancellation has been acknowledged, requiring
  an explicit `reset()`;
* nested transaction with a conflicting isolation level;
* nested transaction whose `SAVEPOINT` fails server-side;
* cancelled `BEGIN` followed by pool release and re-acquisition.

The four tests that exercise the bug fail on the unpatched baseline.  The
cancellation test fails with the observer seeing zero rows while the next
transaction reported success, which is the silent uncommitted-write symptom from the
issue; later rollback or close can discard it.  The nested tests pass on both versions and protect against clearing
the outer transaction.

## Local verification

```
flake8 asyncpg/transaction.py tests/test_transaction.py
python -m unittest -v tests.test_transaction      # 12 tests, OK
python -m unittest -v tests.test_pool             # 66 tests, OK, 4 skipped
python -m unittest -v tests.test_cancellation     #  4 tests, OK
python -m unittest -v tests.test_timeout          # 19 tests, OK
python -m unittest -v tests.test__sourcecode      #  2 tests, OK, 1 skipped
```

The unpatched baseline fails `tests.test_transaction` with 4 failures; the
patched tree passes all 12 tests.

A deterministic server-side `ErrorResponse` for `BEGIN` itself was not added:
PostgreSQL does not normally reject `BEGIN`, and the states in which it does
(for example, an already-aborted transaction) are intercepted by the existing
`is_in_transaction()` check before a command is sent.  The nested
`SAVEPOINT`-failure test covers a real server `ErrorResponse` travelling
through the same `start()` cleanup path.
