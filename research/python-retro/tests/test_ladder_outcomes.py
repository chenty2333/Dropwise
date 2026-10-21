"""No-service regression tests for the ladder outcome interface.

These tests use fake scenarios only; they do not need a database, HTTP server
or network.  They pin the property that a failed B2/B3 operation (returned
``OpResult(ok=False)``, an explicit ``{"ok": False}`` mapping, an exception or
a timeout) is a violation, and that a setup/target failure is not silently
turned into a valid ladder.
"""

from __future__ import annotations

import asyncio
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
RETRO = REPO / "research" / "python-retro"
if str(RETRO) not in sys.path:
    sys.path.insert(0, str(RETRO))

from dropwise_py.engine import (  # noqa: E402
    Expect,
    OpResult,
    Scenario,
    _allows,
    level_retry,
    level_b1,
    explore_observed,
    normalize,
    operation_failed,
    Verdict,
)


class FakeScenario(Scenario):
    failure: str = "none"
    setup_failure: bool = False
    target_failure: bool = False

    def __init__(
        self,
        *,
        failure: str = "none",
        setup_failure: bool = False,
        target_failure: bool = False,
    ) -> None:
        self.failure = failure
        self.setup_failure = setup_failure
        self.target_failure = target_failure

    async def setup(self) -> None:
        if self.setup_failure:
            raise RuntimeError("setup failed")

    async def target(self):
        if self.target_failure:
            raise RuntimeError("target failed")
        await asyncio.sleep(0)
        return "target"

    async def same(self):
        return await self._op()

    async def different(self):
        return await self._op()

    async def _op(self):
        if self.failure == "opresult":
            return OpResult(False, "commit", {"begin": ("err", "boom")}, "commit failed")
        if self.failure == "dict":
            return {"ok": False, "stage": "commit", "error": "boom"}
        if self.failure == "raise":
            raise RuntimeError("boom")
        if self.failure == "timeout":
            await asyncio.sleep(30)
        return OpResult(True, "commit", {"write": ("ok", 1)})

    async def survivor(self):
        return None

    async def observe(self):
        return None

    async def close(self) -> None:
        pass


def factory(**kwargs):
    return lambda: FakeScenario(**kwargs)


class OutcomeInterfaceTests(unittest.IsolatedAsyncioTestCase):
    async def test_opresult_failure_is_violation(self):
        report = await level_retry(
            "fake", factory(failure="opresult"), level="B2", retry="same", reps=1, timeout=1.0
        )
        self.assertEqual(len(report.violations()), 1)
        self.assertEqual(report.trials[0].observation["ok"], False)

    async def test_dict_failure_is_violation(self):
        report = await level_retry(
            "fake", factory(failure="dict"), level="B2", retry="same", reps=1, timeout=1.0
        )
        self.assertEqual(len(report.violations()), 1)

    async def test_exception_is_violation(self):
        report = await level_retry(
            "fake", factory(failure="raise"), level="B2", retry="same", reps=1, timeout=1.0
        )
        self.assertEqual(len(report.violations()), 1)

    async def test_timeout_is_violation(self):
        report = await level_retry(
            "fake", factory(failure="timeout"), level="B2", retry="same", reps=1, timeout=0.2
        )
        self.assertEqual(len(report.violations()), 1)

    async def test_success_is_not_violation(self):
        report = await level_retry(
            "fake", factory(failure="none"), level="B2", retry="same", reps=1, timeout=1.0
        )
        self.assertEqual(len(report.violations()), 0)
        self.assertEqual(report.trials[0].observation["ok"], True)

    async def test_setup_failure_raises_not_silent(self):
        with self.assertRaises(RuntimeError):
            await level_retry(
                "fake",
                factory(setup_failure=True),
                level="B2",
                retry="same",
                reps=1,
                timeout=1.0,
            )

    async def test_target_failure_is_recorded_baseline(self):
        report = await level_retry(
            "fake",
            factory(target_failure=True),
            level="B2",
            retry="same",
            reps=1,
            timeout=1.0,
        )
        self.assertTrue(report.baseline_errors)
        self.assertEqual(report.target_yields, 0)

    def test_operation_failed_protocol(self):
        self.assertTrue(operation_failed(OpResult(False, "x")))
        self.assertFalse(operation_failed(OpResult(True, "x")))
        self.assertTrue(operation_failed({"ok": False}))
        self.assertFalse(operation_failed({"ok": True}))
        self.assertTrue(operation_failed("ERR RuntimeError: x"))
        self.assertFalse(operation_failed(None))

    def test_normalize_opresult(self):
        got = normalize(OpResult(False, "commit", {"v": 1}, "boom"))
        self.assertEqual(
            got, {"ok": False, "stage": "commit", "value": {"v": 1}, "error": "boom"}
        )

    def test_expect_atomic_allows_pre_post_mixed_only(self):
        self.assertTrue(_allows(Expect.ATOMIC, Verdict.PRE))
        self.assertTrue(_allows(Expect.ATOMIC, Verdict.POST))
        self.assertTrue(_allows(Expect.ATOMIC, Verdict.MIXED))
        self.assertFalse(_allows(Expect.ATOMIC, Verdict.NEITHER))
        self.assertFalse(_allows(Expect.ATOMIC, Verdict.UNKNOWN))


class BaselineAndCoverageTests(unittest.IsolatedAsyncioTestCase):
    async def run_levels(self, factory, plans=None):
        return [
            await level_b1("fake", factory, reps=1, plans=plans),
            await level_retry("fake", factory, level="B2", retry="same", reps=1, plans=plans),
            await level_retry("fake", factory, level="B3", retry="different", reps=1, plans=plans),
            await explore_observed("fake", factory, reps=1, plans=plans),
        ]

    async def test_structured_target_failure_invalidates_baseline(self):
        class Bad(FakeScenario):
            async def target(self):
                await asyncio.sleep(0)
                return OpResult(False, "begin", error="failed")
        for report in await self.run_levels(Bad):
            self.assertTrue(report.baseline_errors)
            self.assertFalse(report.exhaustive)
            self.assertEqual(report.trials, [])

    async def test_later_post_calibration_failure_is_not_an_endpoint(self):
        calls = 0
        class BadPost(FakeScenario):
            async def target(self):
                nonlocal calls
                calls += 1
                await asyncio.sleep(0)
                if calls > 1:
                    raise RuntimeError("post failed after successful discovery")
                return "ok"
        report = await explore_observed("fake", BadPost, reps=1)
        self.assertTrue(report.baseline_errors)
        self.assertFalse(report.exhaustive)
        self.assertEqual(report.trials, [])

    async def test_unreached_cut_cannot_produce_a_violation(self):
        for report in await self.run_levels(lambda: FakeScenario(failure="raise"), plans=[99]):
            self.assertEqual(len(report.unrealized()), 1)
            self.assertEqual(report.violations(), [])
            self.assertFalse(report.exhaustive)

    async def test_empty_plan_list_does_not_run_default_search(self):
        for report in await self.run_levels(FakeScenario, plans=[]):
            self.assertEqual(report.trials, [])


if __name__ == "__main__":
    unittest.main()
