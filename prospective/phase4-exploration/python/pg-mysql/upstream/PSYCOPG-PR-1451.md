# psycopg PR #1451 draft

## Title

fix: recover async transaction state when entry is cancelled

## Body

If the task entering an `AsyncConnection.transaction()` block is cancelled
while waiting for `BEGIN` (or `SAVEPOINT`) to complete, `__aenter__` raises
`CancelledError` but the transaction has already been pushed on the
connection-local stack and, depending on timing, started on the server.

The connection remains usable, but `_num_transactions` is left incremented. A
retry is therefore run as a nested transaction (`SAVEPOINT`): it reports
success, but its changes are rolled back together with the abandoned outer
transaction and are never visible from another connection. `Connection.rollback()`
also raises `ProgrammingError` because the stale stack makes the connection
look like it is still inside a transaction block.

`AsyncTransaction.__aenter__` now recovers while it still holds the connection
lock: it inspects the server transaction status and either rolls back the
transaction or savepoint, drops only the local frame if the server transaction
does not belong to us (a nested savepoint failed in an aborted transaction) or
was never started (`IDLE`), or closes the connection if the wire state is
unknown, a command is still in progress, or the recovery is itself cancelled.
The original cancellation or error is always re-raised. If the caller owns an
outer transaction, explicit or implicit (autocommit off), only the entry
savepoint is rolled back and the caller's transaction is preserved.

Fixes #1451.

## Tests

Added to `tests/test_transaction_async.py`:

- cancellation before `BEGIN` is sent;
- cancellation after `BEGIN` was executed but before its reply was consumed,
  with the server state confirmed as `idle in transaction` from an independent
  connection;
- cancellation of a nested `SAVEPOINT` entry, checking that the outer
  transaction stays open and its changes commit visibly;
- cancellation of an entry which is a savepoint in an existing implicit
  transaction (autocommit off), checking that prior work is preserved and only
  committed by an explicit `commit()`;
- cancellation of the recovery itself, checking that the connection is closed
  and cannot be reused;
- a `SAVEPOINT` rejected by the server in an aborted transaction, checking
  that the local stack stays balanced.

The six entry-recovery regressions fail on the unpatched baseline. The
connection-lock cancellation control passes both versions. A further control
attempts to re-enter the same active transaction object, catches the expected
TypeError and verifies that the original writes still commit. It passes with
pipeline both off and on; entry recovery must not unwind the original frame
when a second entry is rejected.

## Validation

```
PSYCOPG_TEST_DSN=... pytest tests/test_transaction_async.py tests/test_transaction.py
177 passed, 9 skipped
```

`black --check`, `flake8`, and `mypy --follow-imports=silent` are clean on the
changed files.

Broader async suite (`test_connection_async`, `test_cursor_*_async`,
`test_pipeline_async`, `test_tpc_async`, `test_prepared_async`,
`test_concurrency_async`, `test_notify_async`): 784 passed, 37 skipped when
excluding `test_resolve_error_host`, which fails independently of this change:
the local resolver maps `nosuchhost.example.com` to a non-public address
instead of failing DNS resolution as the test expects.

The local environment used psycopg's binary libpq implementation. Not run:
full test suite, Windows/macOS, separate pure-Python and psycopg-c matrices,
remote CI.
