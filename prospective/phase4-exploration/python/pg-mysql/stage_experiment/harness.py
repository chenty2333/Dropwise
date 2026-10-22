"""Comparison Harness: Yield-Index Replay (Method A) vs Stage-Targeted Cancellation (Method B).

Implements:
1. PureTask-based stepped execution capturing yield indices and classified stages.
2. Method A: Replay at each yield index k in 1..K.
3. Method B: Directed cancellation targeting classified semantic stages.
4. Independent observer state verification (pg_stat_activity, table rows, successor check).
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import sys
import time
from dataclasses import dataclass
from typing import Any, Callable, Coroutine

from asyncio.tasks import _PyTask as PureTask

import psycopg
from psycopg import errors as pe
from psycopg import pq

from stage_experiment.stage_definitions import Stage, StageClassifier


@dataclass
class TrialResult:
    method: str                    # "yield" or "stage"
    target_spec: Any               # k (int) or Stage
    realized_yield: int | None
    realized_stage: Stage | None
    outcome: str                   # "CANCELLED", "COMPLETED", "EXCEPTION", "TIMEOUT"
    exc_type: str | None
    driver_closed: bool
    conn_tx_status: int | None
    successor_ok: bool
    observer_rows: int
    verdict: str                   # "PRE", "POST", "FAIL_CLOSED", "CORRUPT", "UNKNOWN"
    duration_s: float


class StageSteppingTask(PureTask):
    def __init__(
        self,
        coro: Coroutine[Any, Any, Any],
        *,
        mode: str,                       # "count", "yield", "stage"
        target_cut: Any = None,          # int (yield) or Stage (stage)
        context_holder: dict[str, Any],  # {"method_context": ..., "conn": ..., "tracker": ...}
        on_cut_hook: Callable[[], None] | None = None,
        second_cut_stage: Stage | None = None,
    ) -> None:
        self.mode = mode
        self.target_cut = target_cut
        self.context_holder = context_holder
        self.on_cut_hook = on_cut_hook
        self.second_cut_stage = second_cut_stage

        self.yields = 0
        self.cut_requested = False
        self.cut_yield: int | None = None
        self.cut_stage: Stage | None = None
        self.second_requested = False
        self.trace: list[tuple[int, Stage, str | None, int | None, int | None]] = []

        super().__init__(coro)

    def _get_current_state(self) -> tuple[Stage, str | None, int | None, int | None]:
        method_ctx = self.context_holder.get("method_context")
        conn = self.context_holder.get("conn")
        tracker = self.context_holder.get("tracker")

        tx_status = conn.pgconn.transaction_status if (conn and hasattr(conn, "pgconn")) else None
        io_readiness = tracker.last_wait if (tracker and hasattr(tracker, "last_wait")) else None

        stage = StageClassifier.classify(method_ctx, tx_status, io_readiness)
        return stage, method_ctx, tx_status, io_readiness

    def _Task__step(self, exc: BaseException | None = None) -> None:
        super()._Task__step(exc)
        if self.done():
            return

        self.yields += 1
        stage, method_ctx, tx_status, io_readiness = self._get_current_state()
        self.trace.append((self.yields, stage, method_ctx, tx_status, io_readiness))

        if not self.cut_requested:
            trigger = False
            if self.mode == "yield" and self.yields == self.target_cut:
                trigger = True
            elif self.mode == "stage" and stage == self.target_cut:
                trigger = True

            if trigger:
                self.cut_requested = True
                self.cut_yield = self.yields
                self.cut_stage = stage
                if self.on_cut_hook:
                    with contextlib.suppress(BaseException):
                        self.on_cut_hook()
                self.cancel()
                return

        # Double cancel support
        if self.cut_requested and not self.second_requested and self.second_cut_stage:
            if stage == self.second_cut_stage:
                self.second_requested = True
                self.cancel()


async def execute_trial(
    scenario_runner: Callable[[dict[str, Any]], Coroutine[Any, Any, Any]],
    *,
    mode: str,
    target_cut: Any,
    dsn: str,
    table_name: str,
    on_cut_hook: Callable[[], None] | None = None,
    second_cut_stage: Stage | None = None,
    observer_conn: psycopg.AsyncConnection,
) -> tuple[TrialResult, list[tuple[int, Stage, str | None, int | None, int | None]]]:
    started = time.monotonic()
    context_holder: dict[str, Any] = {
        "method_context": None,
        "conn": None,
        "tracker": None,
        "table": table_name,
        "dsn": dsn,
    }

    # Clean table state before trial
    await asyncio.wait_for(
        observer_conn.execute(f"DELETE FROM {table_name}"),
        timeout=3.0,
    )

    task = StageSteppingTask(
        scenario_runner(context_holder),
        mode=mode,
        target_cut=target_cut,
        context_holder=context_holder,
        on_cut_hook=on_cut_hook,
        second_cut_stage=second_cut_stage,
    )

    outcome = "UNKNOWN"
    exc_type = None
    target_exc = None
    try:
        await asyncio.wait_for(task, timeout=8.0)
        outcome = "COMPLETED"
    except asyncio.CancelledError as ce:
        outcome = "CANCELLED"
        target_exc = ce
        exc_type = "CancelledError"
    except asyncio.TimeoutError as te:
        outcome = "TIMEOUT"
        target_exc = te
        exc_type = "TimeoutError"
        task.cancel()
        with contextlib.suppress(BaseException):
            await task
    except BaseException as be:
        outcome = "EXCEPTION"
        target_exc = be
        exc_type = type(be).__name__

    conn: psycopg.AsyncConnection | None = context_holder.get("conn")
    driver_closed = conn.closed if conn else True
    conn_tx_status = conn.pgconn.transaction_status if (conn and not driver_closed) else None

    # Check successor behavior on the tested handle
    successor_ok = False
    if conn and not driver_closed:
        try:
            cur = await asyncio.wait_for(conn.execute("SELECT 1"), timeout=2.0)
            res = await cur.fetchone()
            successor_ok = (res == (1,))
        except Exception:
            successor_ok = False
    elif conn and driver_closed:
        # For a closed connection, attempting execute should cleanly fail with OperationalError("closed")
        try:
            await asyncio.wait_for(conn.execute("SELECT 1"), timeout=2.0)
            successor_ok = False  # unexpected success on closed handle!
        except pe.OperationalError:
            successor_ok = True  # cleanly rejected

    # Check independent observer state
    cur = await asyncio.wait_for(
        observer_conn.execute(f"SELECT count(*) FROM {table_name}"),
        timeout=2.0,
    )
    row = await cur.fetchone()
    observer_rows = row[0] if row else 0

    # Determine verdict
    # PRE: 0 rows committed, clean rollback or clean fail-closed
    # POST: 1 row committed (valid if cancelled after commit sent/completed)
    # FAIL_CLOSED: driver cleanly severed connection upon interruption
    # CORRUPT: TypeError, stuck in ACTIVE, or corrupt state
    if exc_type == "TypeError" or (conn and not driver_closed and conn_tx_status == pq.TransactionStatus.ACTIVE):
        verdict = "CORRUPT"
    elif outcome == "CANCELLED":
        if driver_closed:
            verdict = "FAIL_CLOSED"
        elif observer_rows == 0:
            verdict = "PRE"
        elif observer_rows > 0:
            verdict = "POST"
        else:
            verdict = "UNKNOWN"
    elif outcome == "COMPLETED":
        verdict = "POST"
    else:
        verdict = "EXCEPTION"

    # Always ensure test connection is closed at end of trial
    if conn and not conn.closed:
        with contextlib.suppress(BaseException):
            await asyncio.wait_for(conn.close(), timeout=2.0)

    duration = time.monotonic() - started
    result = TrialResult(
        method=mode,
        target_spec=target_cut,
        realized_yield=task.cut_yield,
        realized_stage=task.cut_stage,
        outcome=outcome,
        exc_type=exc_type,
        driver_closed=driver_closed,
        conn_tx_status=conn_tx_status,
        successor_ok=successor_ok,
        observer_rows=observer_rows,
        verdict=verdict,
        duration_s=duration,
    )
    return result, task.trace
