"""Narrow fail-closed repair for cancelling psycopg during a partial send.

The target defect (psycopg 3.3.6, binary implementation): in
``AsyncConnection.wait`` a CancelledError caught during a blocked send calls
``_try_cancel()`` and then re-enters ``waiting.wait_async(gen)`` with the
already-started generator.  ``next(gen)`` then resumes the send generator with
``None`` and raises ``TypeError: an integer is required``; the connection stays
ACTIVE/busy and every successor gets "another command is already in progress".

This runtime-only repair does not try to resume the generator.  If that
specific TypeError escapes, it terminates the connection and re-raises
CancelledError: the caller gets the interruption it asked for, and the lease is
explicitly unusable instead of silently stuck.  It is a fail-closed containment,
not an upstream-ready fix; the real repair belongs in the wait/cancel-generator
path.
"""
from __future__ import annotations
import asyncio
import contextlib
import psycopg
from psycopg.connection_async import AsyncConnection

_ORIGINAL = None


def install() -> None:
    global _ORIGINAL
    if _ORIGINAL is not None:
        return
    _ORIGINAL = AsyncConnection.wait

    async def guarded(self, gen, interval=0.1, timeout=None):
        try:
            return await _ORIGINAL(self, gen, interval=interval, timeout=timeout)
        except TypeError as exc:
            if "integer is required" not in str(exc):
                raise
            # The generator is already started and the protocol is mid-send;
            # resuming it is not safe.  Close in the background so this task's
            # cancellation can complete, then surface CancelledError.
            close_task = asyncio.create_task(self.close())
            with contextlib.suppress(BaseException):
                await asyncio.shield(close_task)
            raise asyncio.CancelledError() from exc

    AsyncConnection.wait = guarded
