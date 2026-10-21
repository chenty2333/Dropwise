"""No-service tests for field-level observation contracts.

These use fake scenarios only.  They pin three properties that the single-blob
observation cannot express: per-effect expectations, a state guard, and a
coverage gap (missing or degenerate required effect) that must not read clean.
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

from dropwise_py.engine import Expect, Scenario, Verdict, explore_observed  # noqa: E402
from dropwise_py.fields import (  # noqa: E402
    Field,
    FieldContract,
    FieldContractError,
    FieldKind,
    FieldStatus,
    calibration,
    explore_fields,
    flatten_observation,
    judge_trial,
)


class SplitScenario(Scenario):
    """a completes at once; b only after the first suspension.

    The cancel path can also mark the handle dirty, to exercise a STATE guard.
    """

    def __init__(self, *, dirty_on_cancel: bool = False, hide_observer: bool = False):
        self.dirty_on_cancel = dirty_on_cancel
        self.hide_observer = hide_observer
        self.a = 0
        self.b = 0
        self.clean = True

    async def setup(self) -> None:
        self.a = self.b = 0
        self.clean = True

    async def target(self):
        self.a = 1
        try:
            await asyncio.sleep(0)
        except asyncio.CancelledError:
            if self.dirty_on_cancel:
                self.clean = False
            raise
        self.b = 1

    async def survivor(self):
        return {"a": self.a, "b": self.b,
                "state": "clean" if self.clean else "dirty"}

    async def observe(self):
        return {} if self.hide_observer else {"rows": []}

    async def close(self) -> None:
        pass


def split_factory(**kwargs):
    def make():
        return SplitScenario(**kwargs)
    return make


CONTRACT_MIXED = FieldContract((
    Field("survivor.a", FieldKind.EFFECT, Expect.ATOMIC),
    Field("survivor.b", FieldKind.EFFECT, Expect.ATOMIC),
    Field("survivor.state", FieldKind.STATE),
))
CONTRACT_COMPLETED = FieldContract((
    Field("survivor.a", FieldKind.EFFECT, Expect.ATOMIC),
    Field("survivor.b", FieldKind.EFFECT, Expect.COMPLETED),
    Field("survivor.state", FieldKind.STATE),
))


class FieldJudgingTests(unittest.TestCase):
    def test_flatten_dotted_names(self):
        flat = flatten_observation({"survivor": {"a": 1, "nested": {"b": 2}},
                                    "observer": {"rows": []}})
        self.assertEqual(flat, {"survivor.a": 1, "survivor.nested.b": 2,
                                "observer.rows": []})

    def test_contract_validation(self):
        with self.assertRaises(FieldContractError):
            FieldContract((Field("survivor.state", FieldKind.STATE),))
        with self.assertRaises(FieldContractError):
            FieldContract((Field("x", FieldKind.EFFECT),
                           Field("x", FieldKind.EFFECT)))

    def test_per_field_mixed_allowed_but_blob_is_neither(self):
        pre = [{"survivor.a": 0, "survivor.b": 0, "survivor.state": "clean"}]
        post = [{"survivor.a": 1, "survivor.b": 1, "survivor.state": "clean"}]
        obs = {"survivor.a": 1, "survivor.b": 0, "survivor.state": "clean"}
        judgment = judge_trial(CONTRACT_MIXED, pre, post, obs)
        self.assertIs(judgment.status, FieldStatus.MIXED)
        self.assertEqual(judgment.by_name()["survivor.a"].status, FieldStatus.POST)
        self.assertEqual(judgment.by_name()["survivor.b"].status, FieldStatus.PRE)
        self.assertEqual(judgment.by_name()["survivor.state"].status, FieldStatus.HOLDS)
        self.assertEqual(judgment.violations, [])

        # The same shape under a per-field completed expectation is a violation.
        judgment = judge_trial(CONTRACT_COMPLETED, pre, post, obs)
        self.assertIs(judgment.status, FieldStatus.VIOLATION)

        # A single blob with the same values is neither pre nor post.
        blob_pre = {"survivor": {"a": 0, "b": 0, "state": "clean"}}
        blob_post = {"survivor": {"a": 1, "b": 1, "state": "clean"}}
        blob = {"survivor": {"a": 1, "b": 0, "state": "clean"}}
        flat_pre = flatten_observation(blob_pre)
        flat_post = flatten_observation(blob_post)
        self.assertNotEqual(flatten_observation(blob), flat_pre)
        self.assertNotEqual(flatten_observation(blob), flat_post)

    def test_uncovered_required_field(self):
        contract = FieldContract((
            Field("survivor.a", FieldKind.EFFECT),
            Field("observer.rows", FieldKind.EFFECT),
        ))
        pre = [{"survivor.a": 0, "observer.rows": []}]
        post = [{"survivor.a": 1, "observer.rows": []}]
        obs = {"survivor.a": 1}
        judgment = judge_trial(contract, pre, post, obs)
        self.assertIs(judgment.by_name()["observer.rows"].status,
                      FieldStatus.UNCOVERED)
        self.assertIs(judgment.status, FieldStatus.UNCOVERED)
        self.assertFalse(judgment.coverage_complete)

    def test_degenerate_required_effect_is_not_clean(self):
        contract = FieldContract((
            Field("survivor.a", FieldKind.EFFECT),
            Field("observer.rows", FieldKind.EFFECT),
        ))
        pre = [{"survivor.a": 0, "observer.rows": []}]
        post = [{"survivor.a": 1, "observer.rows": []}]
        obs = {"survivor.a": 1, "observer.rows": []}
        judgment = judge_trial(contract, pre, post, obs)
        self.assertIs(judgment.by_name()["observer.rows"].status,
                      FieldStatus.DEGENERATE)
        self.assertIs(judgment.status, FieldStatus.DEGENERATE)
        self.assertFalse(judgment.coverage_complete)

    def test_state_guard_violation(self):
        contract = FieldContract((
            Field("survivor.a", FieldKind.EFFECT),
            Field("survivor.state", FieldKind.STATE),
        ))
        pre = [{"survivor.a": 0, "survivor.state": "clean"}]
        post = [{"survivor.a": 1, "survivor.state": "clean"}]
        obs = {"survivor.a": 1, "survivor.state": "dirty"}
        judgment = judge_trial(contract, pre, post, obs)
        self.assertIs(judgment.by_name()["survivor.state"].status,
                      FieldStatus.VIOLATION)
        self.assertIs(judgment.status, FieldStatus.VIOLATION)

    def test_unstable_calibration_is_unknown(self):
        contract = FieldContract((Field("survivor.a", FieldKind.EFFECT),))
        pre = [{"survivor.a": 0}, {"survivor.a": 0}]
        post = [{"survivor.a": 1}, {"survivor.a": 2}]
        judgment = judge_trial(contract, pre, post, {"survivor.a": 1})
        self.assertIs(judgment.status, FieldStatus.UNKNOWN)

    def test_calibration_summary(self):
        info = calibration(CONTRACT_MIXED,
                           [{"survivor.a": 0, "survivor.b": 0,
                             "survivor.state": "clean"}],
                           [{"survivor.a": 1, "survivor.b": 1,
                             "survivor.state": "clean"}])
        self.assertTrue(info["survivor.a"]["discriminates"])
        self.assertFalse(info["survivor.state"]["discriminates"])

    def test_judge_trial_is_synchronous(self):
        # No event loop, no engine: the synchronous psycopg harness can reuse it.
        contract = FieldContract((Field("retry_visible", FieldKind.EFFECT,
                                        Expect.COMPLETED),))
        judgment = judge_trial(contract, [{"retry_visible": False}],
                               [{"retry_visible": True}],
                               {"retry_visible": False})
        self.assertIs(judgment.status, FieldStatus.VIOLATION)


class ExploreFieldsTests(unittest.IsolatedAsyncioTestCase):
    async def test_mixed_effect_allowed_and_completed_violation(self):
        report = await explore_fields(
            "split", split_factory(), CONTRACT_MIXED, reps=1, plans=[1])
        self.assertEqual(len(report.trials), 1)
        trial = report.trials[0]
        self.assertIsNotNone(trial.judgment)
        self.assertIs(trial.judgment.status, FieldStatus.MIXED)
        self.assertEqual(report.violations(), [])

        report = await explore_fields(
            "split-completed", split_factory(), CONTRACT_COMPLETED,
            reps=1, plans=[1])
        self.assertEqual(len(report.trials), 1)
        self.assertIs(report.trials[0].judgment.status, FieldStatus.VIOLATION)
        self.assertEqual(len(report.violations()), 1)

    async def test_blob_reports_neither_for_the_same_trial(self):
        report = await explore_observed(
            "split-blob", split_factory(), expect=Expect.ATOMIC, reps=1, plans=[1])
        self.assertEqual(len(report.trials), 1)
        self.assertIs(report.trials[0].verdict, Verdict.NEITHER)

    async def test_state_guard(self):
        report = await explore_fields(
            "dirty", split_factory(dirty_on_cancel=True), CONTRACT_MIXED,
            reps=1, plans=[1])
        self.assertIs(report.trials[0].judgment.status, FieldStatus.VIOLATION)
        self.assertIs(report.trials[0].judgment.by_name()["survivor.state"].status,
                      FieldStatus.VIOLATION)

    async def test_uncovered_in_full_run(self):
        contract = FieldContract((
            Field("survivor.a", FieldKind.EFFECT),
            Field("observer.rows", FieldKind.EFFECT),
        ))
        report = await explore_fields(
            "hidden", split_factory(hide_observer=True), contract,
            reps=1, plans=[1])
        self.assertIs(report.trials[0].judgment.status, FieldStatus.UNCOVERED)
        self.assertFalse(report.trials[0].judgment.coverage_complete)
        self.assertTrue(any("uncovered" in n for n in report.notes))


if __name__ == "__main__":
    unittest.main()
