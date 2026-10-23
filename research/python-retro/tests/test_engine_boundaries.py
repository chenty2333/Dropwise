"""Oracle and caller-cancellation regressions; no external services required."""

import asyncio
import unittest

from dropwise_py.engine import (
    Expect,
    Scenario,
    _await_target,
    _observed_once,
    _scenario_run,
    _survivor_once,
    count_yields,
    explore_observed,
    level_b1,
    level_retry,
)
from dropwise_py.fields import Field, FieldContract, explore_fields


class QuietScenario(Scenario):
    async def setup(self):
        pass

    async def target(self):
        await asyncio.sleep(0)

    async def survivor(self):
        return 0

    async def observe(self):
        return 0

    async def close(self):
        pass


class EngineBoundaryTests(unittest.IsolatedAsyncioTestCase):
    async def test_target_exceptions_are_distinct_from_deadlines_and_returned_values(self):
        async def raises():
            await asyncio.sleep(0)
            raise TimeoutError("target-owned timeout")

        failure = await _await_target(raises(), None, 1)
        self.assertEqual(failure.outcome, "error")
        self.assertIn("target-owned timeout", failure.error)

        value = RuntimeError("a successfully returned value")

        async def returns():
            return value

        success = await _await_target(returns(), None, 1)
        self.assertEqual(success.outcome, "completed")
        self.assertIs(success.result, value)

    async def test_a_previously_handled_cancellation_is_not_a_new_external_cancel(self):
        caller = asyncio.current_task()
        caller.cancel()
        try:
            await asyncio.sleep(0)
        except asyncio.CancelledError:
            pass
        self.assertEqual(caller.cancelling(), 1)
        try:
            result = await _await_target(QuietScenario().target(), 1, 1)
            self.assertEqual(result.outcome, "cancelled")
            self.assertTrue(result.realized)
            self.assertEqual(caller.cancelling(), 1)
        finally:
            caller.uncancel()

    async def test_a_pending_caller_cancellation_is_not_an_injected_cut(self):
        caller = asyncio.current_task()
        caller.cancel()
        try:
            with self.assertRaises(asyncio.CancelledError):
                await _await_target(QuietScenario().target(), 1, 1)
        finally:
            caller.uncancel()

    async def test_equal_calibrations_satisfy_both_endpoint_contracts(self):
        for expect in Expect:
            with self.subTest(expect=expect):
                report = await explore_observed(
                    "equal", QuietScenario, expect=expect, reps=1
                )
                self.assertEqual(report.violations(), [])
                self.assertTrue(any("degenerate" in note for note in report.notes))

    async def test_external_cancellation_propagates_at_each_stage(self):
        for stage in ("target", "survivor", "observer", "retry", "close"):
            with self.subTest(stage=stage):
                ready = asyncio.Event()

                async def wait():
                    ready.set()
                    await asyncio.Future()

                class WaitingScenario(QuietScenario):
                    async def target(self):
                        if stage == "target":
                            await wait()
                        await super().target()

                    async def survivor(self):
                        if stage == "survivor":
                            await wait()
                        return 0

                    async def observe(self):
                        if stage == "observer":
                            await wait()
                        return 0

                    async def same(self):
                        await wait()

                    async def close(self):
                        if stage == "close":
                            await wait()

                if stage == "target":
                    coro = count_yields(WaitingScenario, timeout=1)
                elif stage == "retry":
                    coro = _survivor_once(
                        WaitingScenario, cut_after=1, retry="same", timeout=1
                    )
                elif stage == "close":
                    coro = _scenario_run(WaitingScenario, lambda sc: sc.observe(), 1)
                else:
                    coro = _observed_once(
                        WaitingScenario, run_target=False, cut_after=None, timeout=1
                    )
                task = asyncio.create_task(coro)
                await asyncio.wait_for(ready.wait(), 1)
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await task

    async def test_invalid_search_parameters_fail_before_running_the_scenario(self):
        def forbidden():
            self.fail("invalid parameters must be rejected before setup")

        for kwargs in ({"reps": 0}, {"reps": -1}, {"timeout": 0},
                       {"timeout": float("nan")}, {"plans": [0]}, {"plans": [-1]}):
            runners = (
                level_b1("invalid", forbidden, **kwargs),
                level_retry("invalid", forbidden, level="B2", retry="same", **kwargs),
                explore_observed("invalid", forbidden, **kwargs),
                explore_fields("invalid", forbidden,
                               contract=FieldContract((Field("survivor"),)), **kwargs),
            )
            for runner in runners:
                with self.subTest(kwargs=kwargs, runner=runner.cr_code.co_name):
                    with self.assertRaises(ValueError):
                        await runner
