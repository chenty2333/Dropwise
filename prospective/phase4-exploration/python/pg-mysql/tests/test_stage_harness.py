"""Negative controls for observation ordering, replay validity and state coverage."""

import asyncio
import unittest
from dataclasses import replace

from stage_experiment.harness import compare, execute_trial, summarize
from stage_experiment.stage_definitions import Stage


class Connection:
    def __init__(self, *, wrong_result=False):
        self.closed = False
        self.wrong_result = wrong_result

    def is_closed(self):
        return self.closed

    def is_in_transaction(self):
        return False

    def get_server_pid(self):
        return 123

    async def fetch(self, query):
        assert not self.closed
        return [(43 if self.wrong_result else 42,)]

    async def close(self):
        self.closed = True


class Observer:
    async def execute(self, query):
        pass

    async def fetch(self, query):
        if "pg_stat_activity" in query:
            return [("idle", False)]
        if "pg_locks" in query:
            return [(0,)]
        return []


def runner_for(conn, *, second=False):
    async def prepare(holder):
        holder.update(conn=conn, driver="asyncpg", expected_rows=[])

    async def runner(holder):
        holder.update(operation="outer.fetch", method_context="body")
        try:
            await asyncio.sleep(0)
        except asyncio.CancelledError:
            if second:
                # Select the real recovery marker in this synthetic control.
                holder["driver"] = "psycopg"
                holder["method_context"] = "recovery"
                try:
                    await asyncio.sleep(0)
                except asyncio.CancelledError:
                    holder["second_delivered"] = True
                    raise
            raise

    runner.prepare = prepare
    return runner


class HarnessTests(unittest.IsolatedAsyncioTestCase):
    async def trial(self, *, mode="yield", cut=1, second=False, wrong=False):
        conn = Connection(wrong_result=wrong)
        result, trace = await execute_trial(
            runner_for(conn, second=second),
            mode=mode,
            target_cut=cut,
            dsn="unused",
            table_name="test_table",
            observer_conn=Observer(),
            second_cut_stage=Stage.RECOVERY_DRAIN if second else None,
        )
        self.assertTrue(conn.closed)  # Harness teardown did happen, after snapshot.
        return result, trace

    async def test_harness_close_not_driver_close(self):
        result, _ = await self.trial()
        self.assertFalse(result.driver_closed)
        self.assertEqual(result.verdict, "PRE")
        self.assertTrue(result.successor_ok)

    async def test_wrong_successor_is_a_failure(self):
        result, _ = await self.trial(wrong=True)
        self.assertEqual(result.verdict, "FAILURE")
        self.assertIn("successor_or_closed_rejection", result.failures)

    async def test_unreached_target_is_unknown(self):
        result, _ = await self.trial(cut=99)
        self.assertFalse(result.valid)
        self.assertEqual(result.verdict, "UNKNOWN")

    async def test_second_cancel_is_delivered(self):
        result, _ = await self.trial(second=True)
        self.assertTrue(result.second_requested)
        self.assertTrue(result.second_delivered)
        self.assertTrue(result.valid)
        self.assertEqual(result.outcome, "CANCELLED")

    async def test_swallowed_cancel_is_not_a_clean_completion(self):
        base = runner_for(Connection())

        async def runner(holder):
            try:
                await base(holder)
            except asyncio.CancelledError:
                pass

        runner.prepare = base.prepare
        result, _ = await execute_trial(
            runner,
            mode="yield",
            target_cut=1,
            dsn="unused",
            table_name="test_table",
            observer_conn=Observer(),
        )
        self.assertIn("cancellation_not_propagated", result.failures)
        self.assertEqual(result.verdict, "FAILURE")

    async def test_absent_second_cancel_not_counted(self):
        conn = Connection()
        result, _ = await execute_trial(
            runner_for(conn),
            mode="yield",
            target_cut=1,
            dsn="unused",
            table_name="test_table",
            observer_conn=Observer(),
            second_cut_stage=Stage.RECOVERY_DRAIN,
        )
        self.assertFalse(result.second_requested)
        self.assertFalse(result.second_delivered)
        self.assertFalse(result.valid)

    async def test_equal_labels_do_not_hide_state_or_operation(self):
        calibration, trace = await self.trial(mode="count", cut=None)
        a, _ = await self.trial()
        for b in (
            replace(a, operation="inner.fetch"),
            replace(a, observer_rows=[1]),
            replace(a, successor_ok=False),
            replace(a, conn_tx_status=2),
        ):
            summary = summarize(calibration, trace, [a], [b])
            self.assertTrue(summary["missing_observations"])
            self.assertFalse(summary["observed_state_coverage_preserved"])

    async def test_failed_or_empty_calibration_cannot_pass(self):
        calibration, trace = await self.trial(mode="count", cut=None)
        a, _ = await self.trial()
        for result, events in (
            (replace(calibration, outcome="EXCEPTION"), trace),
            (calibration, []),
        ):
            summary = summarize(result, events, [a], [a])
            self.assertFalse(summary["valid"])
            self.assertFalse(summary["observed_state_coverage_preserved"])

    async def test_invalid_trial_cannot_claim_coverage(self):
        calibration, trace = await self.trial(mode="count", cut=None)
        a, _ = await self.trial()
        summary = summarize(calibration, trace, [a], [replace(a, valid=False)])
        self.assertFalse(summary["valid"])
        self.assertFalse(summary["observed_state_coverage_preserved"])


class SetupFailureTests(unittest.IsolatedAsyncioTestCase):
    async def test_setup_error_stops_replay_and_stays_unknown(self):
        calls = []

        async def prepare(holder):
            calls.append("prepare")
            raise OSError("database unavailable")

        async def runner(holder):
            self.fail("failed setup must not run the scenario")

        runner.prepare = prepare
        result = await compare(
            runner, dsn="unused", table_name="test_table", observer_conn=Observer()
        )
        self.assertEqual(calls, ["prepare"])
        self.assertFalse(result["valid"])
        self.assertEqual(result["calibration"]["verdict"], "UNKNOWN")
        self.assertIsNone(result["calibration"]["driver_closed"])
        self.assertEqual(result["method_a"]["trials"], 0)
        self.assertEqual(result["method_b"]["trials"], 0)
