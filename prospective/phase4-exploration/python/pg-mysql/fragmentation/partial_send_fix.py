"""Fail-closed repair for cancelling psycopg during a partial send.

Inspects the generator's actual wait stage via a lightweight wrapper:
if the generator is waiting to write (WAIT_W) or has not yet reached the
read readiness stage, cancellation cannot safely resume the send generator
or drain from the server. The connection is closed fail-closed immediately.
"""

from __future__ import annotations

import asyncio
from typing import Any
import psycopg
from psycopg import waiting
from psycopg.connection_async import AsyncConnection


class _WaitGenTracker:
    __slots__ = ("_gen", "last_wait")

    def __init__(self, gen: Any) -> None:
        self._gen = gen
        self.last_wait: waiting.Wait | int | None = None

    def __iter__(self) -> _WaitGenTracker:
        return self

    def __next__(self) -> Any:
        s = next(self._gen)
        self.last_wait = s
        return s

    def send(self, val: Any) -> Any:
        s = self._gen.send(val)
        self.last_wait = s
        return s

    def throw(self, *args: Any) -> Any:
        return self._gen.throw(*args)

    def close(self) -> None:
        self._gen.close()


_ORIGINAL = None


def install() -> None:
    global _ORIGINAL
    if _ORIGINAL is not None:
        return
    _ORIGINAL = AsyncConnection.wait

    async def guarded(self, gen, interval=0.1, timeout=None):
        tracker = _WaitGenTracker(gen)
        try:
            return await waiting.wait_async(
                tracker, self.pgconn.socket, interval=interval, timeout=timeout
            )
        except (asyncio.CancelledError, KeyboardInterrupt, SystemExit):
            if self.pgconn.transaction_status == psycopg.pq.TransactionStatus.ACTIVE:
                if tracker.last_wait is None or (tracker.last_wait & waiting.WAIT_W):
                    self.pgconn.finish()
                    raise
            # If in fetch stage, delegate to original recovery
            return await _ORIGINAL(self, tracker, interval=interval, timeout=timeout)

    AsyncConnection.wait = guarded
