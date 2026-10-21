# aio: Abort the connection when an in-flight command is cancelled

## Summary

Cancelling an in-flight command on an `asyncio` connection can leave the
server's reply in the socket while the connection still looks reusable. The
next command then reads that reply as its own result:

```python
cur = await conn.cursor(buffered=True)
task = asyncio.create_task(cur.execute("SELECT SLEEP(0.5) AS cancelled_value"))
# ... wait until SHOW PROCESSLIST reports the query ...
task.cancel()
await task

await cur.execute("SELECT 42 AS next_value")
await cur.fetchall()
# Before: [(0,)], column_names == ('cancelled_value',)
# After:  OperationalError: MySQL Connection not available
```

Fixes #112. The same window exists for unbuffered cursors, fetches, multi-result
sets, and prepared statements, because they all read from the same stream.

## Root cause

The driver only sets `unread_result = True` after it has read a result header,
the column definitions, and the metadata-terminating EOF packet. A cancelled
read can happen before or during any of those packets. The cancellation
propagates out of `MySQLSocket.read()` without changing the transport state,
and the server continues to send the reply for the cancelled command. A later
command writes its own request and reads the cancelled command's reply as if it
were its own.

A cancelled write has a similar problem: only part of a packet may have reached
the server, so the write position is no longer known either. In addition,
`NetworkBrokerCompressed.write()` converted `asyncio.CancelledError` into
`WriteTimeoutError`, so a cancellation during a compressed write was reported
as an unrelated timeout.

## Fix

`MySQLSocket.read()` and `MySQLSocket.write()` are the single choke point used
by query headers, columns, rows, multi-results, prepared statements, cursor
fetches and authentication. Cancellation is handled there:

- On `asyncio.CancelledError`, the socket is marked poisoned, the transport is
  closed/aborted synchronously (awaiting while a cancellation is unwinding
  could be cancelled again before the transport is actually gone), and the
  same `CancelledError` is re-raised. It is never converted to another
  exception.
- Any later read or write on that socket raises
  `OperationalError("MySQL Connection not available")` before touching the
  stream. `is_socket_connected()` also reports `False`.
- `handle_unread_result()` refuses to act on a pending result when the socket
  is gone. The `unread_result` flag is no longer trusted to mean that there is
  a usable reply waiting.
- Cursor cleanup does not try to consume a result from a transport that is
  already gone, and a cursor context manager does not replace a
  `CancelledError` being unwound with a cleanup `InternalError`.
- `NetworkBrokerCompressed.write()` no longer converts `CancelledError` into
  `WriteTimeoutError`; cancellation reaches the socket guard unchanged.

### What this does not do

This does not try to recover the connection by sending `ROLLBACK`, `PING`,
`reset_session()` or another command after a cancellation. There is no way to
prove that the first reply consumed by such a command belongs to the recovery
command rather than to the cancelled one. Recovery is therefore explicit:
close the connection and reconnect. Attempts to reuse a poisoned connection
(including `rollback()` and `reset_session()`) fail with `OperationalError`
instead of returning a false success.

Ordinary read/write timeouts keep their existing behaviour: the existing
decorator closes the connection, and this change does not mark the socket
poisoned for a timeout.

## Tests

New tests in `tests/test_aio_connection.py`
(`MySQLAioCancellationTests`) exercise cancellation at several levels. Most
run against a real MySQL server. They use an independent observer connection
and `SHOW PROCESSLIST` (keyed by `connection_id` and the SQL text) to prove
that the command reached the server before cancelling the task.
Post-cancellation operations are bounded with `asyncio.wait_for` so a stale
stream fails the test instead of hanging it.

Covered paths:

1. Cancelled buffered and unbuffered `execute("SELECT SLEEP(0.5) ...")`
   followed by `SELECT 42`. The follow-up must raise `OperationalError`; the
   cancelled value/column must not appear. On the unpatched tree this returned
   `[(0,)]` / `('cancelled_value',)`.
2. Cancellation while fetching rows from a slowly streamed result, including
   the case where the `CancelledError` unwinds through a cursor context
   manager.
3. Cancelled `nextset()` for a multiple-statement result and a cancelled
   prepared-statement execution; both go through the same socket guard.
4. Controls: normal buffered and unbuffered queries, `close()` after
   cancellation, and a fresh `aio.connect()` returning the correct
   `[(42,)]` / `('next_value',)`.
5. Existing timeout tests still pass.
6. After cancellation, a follow-up `execute`, `rollback()` and
   `reset_session()` all raise `OperationalError`.

The rest of the class runs against a real MySQL server. One additional
unit-level test drives `MySQLSocket.read()` and `MySQLSocket.write()` with a
cancelling broker and checks that both directions poison the socket and that
follow-up reads/writes fail before reaching the broker. The compressed broker
change has a small unit test which checks that a cancelled write stays a
`CancelledError`. A manual check with `compress=True` against MySQL also showed
the connection invalidated and the next query failing with `OperationalError`.

### Verification performed

On the unpatched tree the new class failed as follows (11 tests, 7.710s,
5 failures and 4 errors):

```
FAIL test_cancelled_buffered_execute_is_not_reused
  the connection was silently reused ... rows=[(0,)], columns=('cancelled_value',)
FAIL test_cancelled_unbuffered_execute_is_not_reused
  the connection was silently reused ... rows=[(0,)], columns=('cancelled_value',)
FAIL test_cancelled_next_result_is_not_reused
  the cancelled result set was returned as a next result: rows=[(0,)], columns=('cancelled_value',)
FAIL test_cancelled_prepared_execute_is_not_reused
  expected OperationalError after cancellation, got InterfaceError: Expected Binary OK packet
FAIL test_cancelled_fetch_is_not_reused
  reading the cancelled result blocked for more than 5s, presumably on a stale server reply
ERROR test_cancelled_fetch_in_cursor_context_preserves_cancellation
  InternalError: Unread result found (replacing CancelledError)
ERROR test_cursor_context_preserves_cancellation_with_unread_result
  InternalError: Unread result found (replacing CancelledError)
ERROR test_compressed_write_cancellation_is_preserved
  WriteTimeoutError: 3024 (replacing CancelledError)
ERROR test_socket_cancellations_poison_the_transport
  AttributeError: 'MySQLTcpSocket' object has no attribute '_is_poisoned'
```

With the fix the same class passes (11 tests, 0.713s). Existing module counts
with the fix on MySQL 9.6.0 / CPython 3.13.13:

- `aio_connection`: 94 tests, OK, 7 skipped
- `aio_cursor`: 75 tests, OK
- `aio_pooling`: 18 tests, OK
- `qa_aio_timeout`: 22 tests, OK, 5 skipped

`black --check` is clean for the four changed files. `isort --check-only` is
clean for the three production files; `tests/test_aio_connection.py` still has
the import-order differences that are already present on trunk. `compileall`
passes for all changed files. No remote CI was run.

## Limitations / coverage boundaries

- This tree contains only the pure-Python `aio` connection (the C extension is
  not built), so the C-extension `aio` path is not covered here.
- Server-side prepared-statement cursor fetches (`cmd_stmt_fetch`) are not
  cancelled in a dedicated test, but they use the same `MySQLSocket.read()`
  guard as the tested prepared execute and multi-result paths.
- Cancellation delivered at an await point outside socket I/O does not poison
  the connection (the stream is still consistent). A cursor context manager
  preserves the cancellation in that case, but calling `close()` directly in a
  `finally` block while such a cancellation unwinds can still raise
  `InternalError` for an unread result, as before. This change does not add a
  `Task.cancelling()`-based guard (Python 3.11+ only; the project supports
  Python 3.10) and does not claim to fix that separate case.
- After a cancelled socket operation the connection is intentionally
  unusable. The supported recovery is `close()` plus a new connection.
