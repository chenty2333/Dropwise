"""Matched cancellation replay with observation strictly before harness cleanup.

This is an observed-state comparison, not a proof of defect coverage. It uses
CPython's Python Task implementation; run its stepping tests on runtime upgrades.
"""

from __future__ import annotations

import asyncio
import contextlib
import time
from dataclasses import asdict, dataclass
from typing import Any

from dropwise_py.engine import PureTask

from stage_experiment.stage_definitions import Stage, StageClassifier


@dataclass
class TrialResult:
    method: str
    target_spec: Any
    realized_yield: int | None
    realized_stage: Stage | None
    operation: str | None
    outcome: str
    exc_type: str | None
    driver_closed: bool | None
    conn_tx_status: int | None
    successor_ok: bool
    observer_rows: list[int]
    server_state: str | None
    server_in_transaction: bool | None
    relation_locks: int | None
    second_requested: bool
    second_delivered: bool
    valid: bool
    failures: list[str]
    verdict: str
    duration_s: float

    def signature(self) -> tuple:
        # Preserve operation identity and independent observations. Equal verdict
        # labels alone must never hide distinct failures or nested body phases.
        return (
            self.operation,
            self.realized_stage,
            self.outcome,
            self.exc_type,
            self.driver_closed,
            self.conn_tx_status,
            self.successor_ok,
            tuple(self.observer_rows),
            self.server_state,
            self.server_in_transaction,
            self.relation_locks,
            self.second_requested,
            self.second_delivered,
            tuple(self.failures),
        )


class StageSteppingTask(PureTask):
    def __init__(
        self,
        coro,
        *,
        mode,
        target_cut=None,
        context_holder,
        on_cut_hook=None,
        second_cut_stage=None,
    ):
        self.mode = mode
        self.target_cut = target_cut
        self.context_holder = context_holder
        self.on_cut_hook = on_cut_hook
        self.second_cut_stage = second_cut_stage
        self.yields = 0
        self.cut_requested = False
        self.cut_yield = None
        self.cut_stage = None
        self.cut_operation = None
        self.second_requested = False
        self.trace = []
        super().__init__(coro)

    def _get_current_state(self):
        holder = self.context_holder
        ctx = holder.get("method_context")
        conn = holder.get("conn")
        tracker = holder.get("tracker")
        tx = (
            conn.pgconn.transaction_status
            if conn is not None and hasattr(conn, "pgconn")
            else None
        )
        io = tracker.last_wait if tracker is not None else None
        stage = StageClassifier.classify(ctx, tx, io)
        # asyncpg exposes API-operation suspension, not libpq send/read phases.
        if holder.get("driver") == "asyncpg":
            stage = {
                "enter": Stage.ENTER_WAIT,
                "body": Stage.BODY_FETCH,
                "commit": Stage.COMMIT_WAIT,
                "rollback": Stage.ROLLBACK_WAIT,
                "sp_enter": Stage.SP_ENTER,
                "sp_release": Stage.SP_RELEASE,
            }.get(ctx, Stage.UNKNOWN)
        return stage, ctx, tx, io

    def _Task__step(self, exc=None):
        super()._Task__step(exc)
        if self.done():
            return
        operation = self.context_holder.get("operation")
        # Connection establishment and harness teardown are outside BOTH methods.
        if operation is None:
            return
        self.yields += 1
        stage, ctx, tx, io = self._get_current_state()
        self.trace.append((self.yields, stage, ctx, tx, io, operation))
        if not self.cut_requested:
            trigger = (self.mode == "yield" and self.yields == self.target_cut) or (
                self.mode == "stage" and (operation, stage) == self.target_cut
            )
            if trigger:
                self.cut_requested = True
                self.cut_yield = self.yields
                self.cut_stage = stage
                self.cut_operation = operation
                if self.on_cut_hook is not None:
                    self.on_cut_hook()
                self.cancel()
                return
        if (
            self.cut_requested
            and not self.second_requested
            and self.second_cut_stage is not None
            and stage == self.second_cut_stage
        ):
            self.second_requested = self.cancel()


async def fetch_rows(conn, sql):
    if hasattr(conn, "fetch"):
        return [tuple(row) for row in await conn.fetch(sql)]
    cur = await conn.execute(sql)
    return await cur.fetchall()


def is_closed(conn):
    return conn.is_closed() if hasattr(conn, "is_closed") else conn.closed


async def execute_trial(
    scenario_runner,
    *,
    mode,
    target_cut,
    dsn,
    table_name,
    observer_conn,
    on_cut_hook=None,
    second_cut_stage=None,
):
    started = time.monotonic()
    holder = {
        "method_context": None,
        "operation": None,
        "conn": None,
        "tracker": None,
        "table": table_name,
        "dsn": dsn,
    }
    task = None
    try:
        await asyncio.wait_for(observer_conn.execute(f"DELETE FROM {table_name}"), 3)
        # Establish and instrument before stepping or starting the query budget.
        await asyncio.wait_for(scenario_runner.prepare(holder), 5)
        conn = holder["conn"]
        pid = (
            conn.get_server_pid()
            if hasattr(conn, "get_server_pid")
            else conn.info.backend_pid
        )
        task = StageSteppingTask(
            scenario_runner(holder),
            mode=mode,
            target_cut=target_cut,
            context_holder=holder,
            on_cut_hook=on_cut_hook,
            second_cut_stage=second_cut_stage,
        )
        exc_type = None
        try:
            await asyncio.wait_for(task, 8)
            outcome = "COMPLETED"
        except asyncio.CancelledError:
            if not task.cancelled():
                raise
            outcome, exc_type = "CANCELLED", "CancelledError"
        except TimeoutError:
            outcome, exc_type = "TIMEOUT", "TimeoutError"
        except Exception as exc:
            outcome, exc_type = "EXCEPTION", type(exc).__name__

        # Freeze driver and server state BEFORE successor probes and close().
        closed = is_closed(conn)
        tx = (
            None
            if closed
            else (
                int(conn.pgconn.transaction_status)
                if hasattr(conn, "pgconn")
                else (2 if conn.is_in_transaction() else 0)
            )
        )
        deadline = time.monotonic() + 3
        while True:
            backend = await asyncio.wait_for(
                fetch_rows(
                    observer_conn,
                    f"SELECT state, xact_start IS NOT NULL FROM pg_stat_activity WHERE pid = {pid}",
                ),
                2,
            )
            if not closed or not backend or time.monotonic() >= deadline:
                break
            await asyncio.sleep(0.02)
        state, in_tx = backend[0] if backend else (None, False)
        locks = await asyncio.wait_for(
            fetch_rows(
                observer_conn,
                f"SELECT count(*) FROM pg_locks WHERE pid = {pid} AND relation = '{table_name}'::regclass",
            ),
            2,
        )
        lock_count = locks[0][0]
        rows = await asyncio.wait_for(
            fetch_rows(observer_conn, f"SELECT id FROM {table_name} ORDER BY id"), 2
        )
        committed = [row[0] for row in rows]

        successor_ok = False
        try:
            successor = await asyncio.wait_for(fetch_rows(conn, "SELECT 42"), 2)
            successor_ok = not closed and successor == [(42,)]
        except Exception as exc:
            # Only the driver's documented closed-handle rejection qualifies.
            import psycopg
            import asyncpg

            successor_ok = closed and isinstance(
                exc, (psycopg.OperationalError, asyncpg.InterfaceError)
            )

        valid = (
            outcome != "TIMEOUT"
            and (mode == "count" or task.cut_requested)
            and (
                second_cut_stage is None
                or (task.second_requested and holder.get("second_delivered", False))
            )
        )
        failures = []
        if not successor_ok:
            failures.append("successor_or_closed_rejection")
        if not closed and tx != 0:
            failures.append("transaction_not_idle")
        if (closed and backend) or in_tx or lock_count:
            failures.append("backend_cleanup_unconfirmed")
        expected = holder.get("expected_rows", [1])
        if outcome == "COMPLETED":
            if committed != expected:
                failures.append("wrong_committed_rows")
        elif committed and (
            task.cut_stage not in (Stage.COMMIT_SEND, Stage.COMMIT_WAIT)
            or committed != expected
        ):
            failures.append("unexpected_committed_rows")
        if outcome == "EXCEPTION":
            failures.append("unexpected_exception")
        if task.cut_requested and outcome == "COMPLETED":
            failures.append("cancellation_not_propagated")
        verdict = (
            "UNKNOWN"
            if not valid
            else (
                "FAILURE"
                if failures
                else "FAIL_CLOSED" if closed else "POST" if committed else "PRE"
            )
        )
        return (
            TrialResult(
                mode,
                target_cut,
                task.cut_yield,
                task.cut_stage,
                task.cut_operation,
                outcome,
                exc_type,
                closed,
                tx,
                successor_ok,
                committed,
                state,
                in_tx,
                lock_count,
                task.second_requested,
                holder.get("second_delivered", False),
                valid,
                failures,
                verdict,
                time.monotonic() - started,
            ),
            task.trace,
        )
    except Exception as exc:
        # Setup/observer failures are missing evidence, never driver closure or
        # a successful zero-trial comparison. The finally block still cleans up.
        return TrialResult(
            method=mode,
            target_spec=target_cut,
            realized_yield=task.cut_yield if task is not None else None,
            realized_stage=task.cut_stage if task is not None else None,
            operation=task.cut_operation if task is not None else None,
            outcome="HARNESS_ERROR",
            exc_type=type(exc).__name__,
            driver_closed=None,
            conn_tx_status=None,
            successor_ok=False,
            observer_rows=[],
            server_state=None,
            server_in_transaction=None,
            relation_locks=None,
            second_requested=bool(task and task.second_requested),
            second_delivered=holder.get("second_delivered", False),
            valid=False,
            failures=["setup_or_observation_failed"],
            verdict="UNKNOWN",
            duration_s=time.monotonic() - started,
        ), (task.trace if task is not None else [])
    finally:
        if task is not None and not task.done():
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await task
        # Observation is complete. No cleanup result is attributed to the driver.
        try:
            conn = holder.get("conn")
            if conn is not None and not is_closed(conn):
                await asyncio.wait_for(conn.close(), 3)
        finally:
            cleanup = holder.get("cleanup")
            if cleanup is not None:
                cleanup()


def summarize(calibration, trace, results_a, results_b):
    valid_calibration = (
        calibration.valid
        and calibration.outcome == "COMPLETED"
        and not calibration.failures
        and bool(trace)
    )
    all_valid = (
        valid_calibration
        and bool(results_a)
        and bool(results_b)
        and all(r.valid for r in results_a + results_b)
    )
    a = {r.signature() for r in results_a if r.valid}
    b = {r.signature() for r in results_b if r.valid}

    def group(results):
        return {
            "trials": len(results),
            "time_s": sum(r.duration_s for r in results),
            "details": [asdict(r) for r in results],
        }

    return {
        "valid": all_valid,
        "calibration": asdict(calibration),
        "K": len(trace),
        "trace": trace,
        "method_a": group(results_a),
        "method_b": group(results_b),
        "unrealized_trials": sum(not r.valid for r in results_a + results_b),
        "unclassified_yields": sum(t[1] == Stage.UNKNOWN for t in trace),
        "missing_observations": sorted(a - b, key=repr),
        "extra_observations": sorted(b - a, key=repr),
        "observed_state_coverage_preserved": all_valid and not (a - b),
        "defect_coverage": "not established",
    }


async def compare(runner, *, dsn, table_name, observer_conn, second_cut_stage=None):
    kwargs = dict(dsn=dsn, table_name=table_name, observer_conn=observer_conn)
    calibration, trace = await execute_trial(
        runner, mode="count", target_cut=None, **kwargs
    )
    if (
        not calibration.valid
        or calibration.outcome != "COMPLETED"
        or calibration.failures
        or not trace
    ):
        return summarize(calibration, trace, [], [])
    targets = list(dict.fromkeys((t[5], t[1]) for t in trace))
    a, b = [], []
    for mode, cuts, results in (
        ("yield", range(1, len(trace) + 1), a),
        ("stage", targets, b),
    ):
        for cut in cuts:
            result, _ = await execute_trial(
                runner,
                mode=mode,
                target_cut=cut,
                second_cut_stage=second_cut_stage,
                **kwargs,
            )
            results.append(result)
    return summarize(calibration, trace, a, b)
