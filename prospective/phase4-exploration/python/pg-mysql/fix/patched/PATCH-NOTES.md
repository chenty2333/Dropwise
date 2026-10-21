# Minimal cancel-poison patch for mysql-connector-python 26.7.0 (aio)

This directory is a **local research experiment** for the round-4
`mysql-connector-python` recovery/fix contrast. It is not an upstream patch and
was not sent anywhere.

* Base: `mysql-connector-python==26.7.0`, copied from
  `~/dropwise-py-work-r3/venv313/lib/python3.13/site-packages/mysql`
  (only `mysql/__init__.py` + `mysql/connector/` are kept; the unrelated
  `mysql/vendor` and `mysql/ai` trees are omitted to keep the copy small).
* Loaded with `fix/patched` first on `PYTHONPATH`; verified with
  `mysql.connector.__file__` (see `logs/run.*/patched-import.txt`).
* Rationale: a `CancelledError` raised while `_send_cmd()` is awaiting
  `self._socket.read()` leaves the server reply in the transport, leaves
  `unread_result == False` and leaves the connection open, so the next command
  reads the previous command's reply.  aiomysql avoids this by closing the
  connection in `Connection._read_packet` / `_close_on_cancel`
  (`aiomysql/connection.py:602-613, 1116`).

## Change 1 — `mysql/connector/aio/network.py`

`MySQLSocket.__init__` gains `_close_reason: Optional[str] = None`.

`MySQLSocket._close_on_cancel()` (new):

```python
def _close_on_cancel(self) -> None:
    self._close_reason = "Cancelled during execution"
    self._is_connected = False
    if self._writer is not None:
        try:
            self._writer.close()
            if self._writer.transport is not None:
                self._writer.transport.abort()
        except Exception:
            pass
```

`MySQLSocket.write()` and `MySQLSocket.read()` catch `asyncio.CancelledError`,
call `_close_on_cancel()` and **re-raise the same CancelledError**.  A
cancelled write is treated the same way because a partial packet may have been
sent.

## Change 2 — `mysql/connector/aio/abstracts.py`

`MySQLConnectionAbstract.handle_unread_result()` refuses to run on a
cancel-poisoned socket:

```python
close_reason = getattr(getattr(self, "_socket", None), "_close_reason", None)
if close_reason:
    task = asyncio.current_task()
    if task is None or not task.cancelling():
        raise InterfaceError(f"{close_reason}; connection is not reusable")
```

The `cancelling()` guard matters: cursor/context cleanup calls
`handle_unread_result()` while the `CancelledError` is being unwound; raising
there would replace the in-flight `CancelledError` with `InterfaceError`
(observed in the pre-guard run `logs/run.gTlHvk` patched B1 cut #2, 3/12
violations).  With the guard, B1 is 0/12 and `CancelledError` is preserved.

## Observable effect

* The cancelled command still raises `CancelledError` (never converted).
* The tested later commands on that handle fail (`InterfaceError` when it reaches
  `handle_unread_result`; `OperationalError: MySQL Connection not available`
  when the first call is `conn.cursor()` or a write on the closed transport).
* `conn.close()` still succeeds; a new `aio.connect()` gives a clean stream.
* `rollback()` / `reset_session()` on the poisoned handle fail loudly instead
  of consuming stale replies; they cannot recover the handle.
* Transport abort initiates disconnection; it is not a client-visible rollback
  acknowledgement. Server-side cleanup timing was not established by this
  patch, and already committed effects are not undone.

## Limits

`Task.cancelling()` is Python 3.11+ only.  A positive cancellation count does not prove that a task is currently
unwinding: the count may stay positive after it catches `CancelledError`.
Such a caller can bypass this particular refusal check. The transport is also
aborted, so bypass alone does not prove stale-stream reuse; the same-task
catch-and-reuse path was not exercised. This is a containment experiment, not
a complete production repair.

## Reconstruct the tested copy

The full third-party copy is not committed. In a disposable directory, install
`mysql-connector-python==26.7.0` with `pip install --no-deps --target <directory>`,
then apply `mysql-connector-26.7.0-cancel.patch` there with `patch -p1`. The
contrast runner expects that directory at `fix/patched` and verifies the loaded
module path. Use a fresh directory; do not patch the shared virtual environment
or apply the diff twice to the existing local experiment.
