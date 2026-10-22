"""Tests for Stage Classifier, Harness Stepping Task, and min_repro Negative Controls."""

from __future__ import annotations

import asyncio
import os
import subprocess as sp
import sys
import unittest

from stage_experiment.stage_definitions import Stage, StageClassifier


class StageClassifierTest(unittest.TestCase):
    def test_classify_enter(self):
        self.assertEqual(
            StageClassifier.classify("enter", 1, 2),  # WAIT_W
            Stage.ENTER_SEND,
        )
        self.assertEqual(
            StageClassifier.classify("enter", 1, 1),  # WAIT_R
            Stage.ENTER_WAIT,
        )

    def test_classify_body(self):
        self.assertEqual(
            StageClassifier.classify("body", 1, 2),  # WAIT_W
            Stage.BODY_SEND,
        )
        self.assertEqual(
            StageClassifier.classify("body", 1, 1),  # WAIT_R
            Stage.BODY_FETCH,
        )

    def test_classify_commit(self):
        self.assertEqual(
            StageClassifier.classify("commit", 1, 2),  # WAIT_W
            Stage.COMMIT_SEND,
        )
        self.assertEqual(
            StageClassifier.classify("commit", 1, 1),  # WAIT_R
            Stage.COMMIT_WAIT,
        )

    def test_classify_savepoint(self):
        self.assertEqual(
            StageClassifier.classify("sp_enter", 1, 1),
            Stage.SP_ENTER,
        )
        self.assertEqual(
            StageClassifier.classify("sp_release", 1, 1),
            Stage.SP_RELEASE,
        )

    def test_classify_recovery(self):
        self.assertEqual(
            StageClassifier.classify("recovery", 1, 1),
            Stage.RECOVERY_DRAIN,
        )

    def test_classify_unknown(self):
        self.assertEqual(
            StageClassifier.classify(None, 0, None),
            Stage.UNKNOWN,
        )


class MinReproNegativeControlsTest(unittest.TestCase):
    def _run_min_repro(self, *args: str) -> sp.CompletedProcess[str]:
        cmd = [
            "/home/ava/dropwise-py-work-r3/venv313/bin/python",
            "-m",
            "fragmentation.min_repro",
            "--port",
            "32919",
            *args,
        ]
        env = dict(os.environ)
        env["PYTHONPATH"] = (
            "prospective/phase4-exploration/python/pg-mysql:"
            "research/python-retro:"
            "/home/ava/Worktrees/psycopg/partial-send/psycopg"
        )
        return sp.run(
            cmd,
            cwd="/home/ava/Desktop/Dropwise",
            capture_output=True,
            text=True,
            env=env,
        )

    def test_negative_control_driver_unclosed(self):
        res = self._run_min_repro("--fix", "--negative-control", "driver-unclosed")
        self.assertEqual(res.returncode, 1, f"Expected returncode 1, got {res.returncode}\n{res.stdout}\n{res.stderr}")
        self.assertIn("FAILURE: driver failed to close connection", res.stdout)

    def test_negative_control_wrong_exception(self):
        res = self._run_min_repro("--fix", "--negative-control", "wrong-exception")
        self.assertEqual(res.returncode, 1, f"Expected returncode 1, got {res.returncode}\n{res.stdout}\n{res.stderr}")
        self.assertIn("FAILURE: target task did not raise CancelledError", res.stdout)

    def test_negative_control_unconfirmed_cleanup(self):
        res = self._run_min_repro("--fix", "--negative-control", "unconfirmed-cleanup")
        self.assertEqual(res.returncode, 1, f"Expected returncode 1, got {res.returncode}\n{res.stdout}\n{res.stderr}")
        self.assertIn("FAILURE: backend cleanup UNCONFIRMED", res.stdout)


if __name__ == "__main__":
    unittest.main()
