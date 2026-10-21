"""Candidate repair for interrupted synchronous transaction entry/exit.

The patch is installed on the *original* psycopg classes at runtime; it does not
edit site-packages.  It is deliberately narrow:

* only ``KeyboardInterrupt`` / ``SystemExit`` are treated as interruptions;
* only the transaction object whose entry/exit was interrupted is repaired;
* if the wire exchange is unfinished (libpq reports ACTIVE / a command in
  progress), the connection is closed: there is no resumable generator any more,
  and a ROLLBACK would be read as the reply to the interrupted command;
* otherwise the library's own rollback generator is run for an outer
  transaction, which pops the local transaction stack and sends ROLLBACK.  A
  nested entry is failed closed: psycopg does not record whether SAVEPOINT was
  sent, so rolling back to a savepoint that does not exist could put the
  caller's outer transaction in error.

The patch is a research prototype, not an upstream-ready diff.  Its purpose is
the fix contrast: the same injected executions must return to a clean handle and
a visible successor effect.
"""

from __future__ import annotations

import contextlib
import logging

import psycopg
from psycopg import pq

logger = logging.getLogger("dw.fix")

_INTERRUPTED = (KeyboardInterrupt, SystemExit)
_orig_enter = None
_orig_exit = None
_installed = False


def _close_quiet(conn) -> None:
    with contextlib.suppress(BaseException):
        conn.close()


def _repair_entry(conn, tx, exc) -> str:
    """Run with the connection lock held.  Returns an action label."""
    if conn.closed:
        return "already-closed"
    if tx.status is not psycopg.Transaction.Status.ACTIVE:
        return "no-active-tx"
    if conn._num_transactions <= 0:
        return "no-local-state"
    if pq.ConnStatus(conn.pgconn.status) is not pq.ConnStatus.OK:
        _close_quiet(conn)
        return "closed-bad-pgconn"
    # An unfinished exchange: libpq still has a command in progress.  The
    # generator that owned it was closed by the exception, so there is nothing
    # safe to resume.
    if pq.TransactionStatus(conn.pgconn.transaction_status) is pq.TransactionStatus.ACTIVE:
        _close_quiet(conn)
        return "closed-unread-reply"
    if not tx._outer_transaction:
        # A nested entry may or may not have sent SAVEPOINT; rolling back to a
        # savepoint that does not exist would put the caller's outer
        # transaction in error.  Without a "was it sent" witness, fail closed.
        _close_quiet(conn)
        return "closed-nested-unknown"
    try:
        conn.wait(tx._rollback_gen(exc))
        return "rolled-back"
    except BaseException as exc2:
        logger.warning("entry repair failed: %s: %s", type(exc2).__name__, exc2)
        _close_quiet(conn)
        return "closed-repair-failed"


def _repair_exit(conn, tx, exc) -> str:
    if conn.closed:
        return "already-closed"
    if pq.ConnStatus(conn.pgconn.status) is not pq.ConnStatus.OK:
        _close_quiet(conn)
        return "closed-bad-pgconn"
    if pq.TransactionStatus(conn.pgconn.transaction_status) is pq.TransactionStatus.ACTIVE:
        _close_quiet(conn)
        return "closed-unread-reply"
    if conn._num_transactions == 0 and \
            pq.TransactionStatus(conn.pgconn.transaction_status) is pq.TransactionStatus.INTRANS:
        # COMMIT was sent (or not) but no reply was consumed; the outcome is
        # ambiguous, which the atomic contract allows.  Roll back so the handle
        # cannot be reused with an open server transaction.  The local stack was
        # already popped by _commit_gen, so do not run _rollback_gen again.
        try:
            conn.wait(conn._exec_command(b"ROLLBACK"))
            return "rolled-back-ambiguous"
        except BaseException as exc2:
            logger.warning("exit repair failed: %s: %s", type(exc2).__name__, exc2)
            _close_quiet(conn)
            return "closed-repair-failed"
    return "no-action"


def install() -> None:
    global _orig_enter, _orig_exit, _installed
    if _installed:
        return
    _orig_enter = psycopg.Transaction.__enter__
    _orig_exit = psycopg.Transaction.__exit__

    def enter(self):
        # Mirror the pinned upstream method, with repair inside the SAME lock
        # scope. Calling _orig_enter first would publish broken state when its
        # with block unwinds, before a wrapper can reacquire the lock.
        with self._conn.lock:
            try:
                self._conn.wait(self._enter_gen())
            except _INTERRUPTED as exc:
                try:
                    self._repair_action = _repair_entry(self._conn, self, exc)
                except BaseException:
                    _close_quiet(self._conn)
                    self._repair_action = "closed-repair-raised"
                raise
        return self

    def exit_(self, exc_type, exc_val, exc_tb):
        with self._conn.lock:
            if self.pgconn.status != pq.ConnStatus.OK:
                self.status = self.Status.FAILED
                return False
            try:
                return self._conn.wait(self._exit_gen(exc_type, exc_val, exc_tb))
            except _INTERRUPTED as exc:
                try:
                    self._repair_action = _repair_exit(self._conn, self, exc)
                except BaseException:
                    _close_quiet(self._conn)
                    self._repair_action = "closed-repair-raised"
                raise

    psycopg.Transaction.__enter__ = enter
    psycopg.Transaction.__exit__ = exit_
    _installed = True


def uninstall() -> None:
    global _installed
    if not _installed:
        return
    psycopg.Transaction.__enter__ = _orig_enter
    psycopg.Transaction.__exit__ = _orig_exit
    _installed = False
