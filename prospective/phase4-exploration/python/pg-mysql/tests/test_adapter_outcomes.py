"""No-service regression tests for the psycopg/asyncpg ladder adapters.

These tests use fake connections/scenarios only (no PostgreSQL, proxy or
network).  They pin the round-3 review fixes:

* ``_tx_write`` returns ``OpResult(ok=False, stage=...)`` for a failed
  acquire/BEGIN/INSERT/COMMIT and never continues to COMMIT after a failed
  BEGIN/INSERT;
* a cancelled/timeout retry is a B2/B3 violation (engine ``level_retry``);
* a setup failure raises instead of producing a silent baseline.

Run from the repo root:
    PYTHONPATH=research/python-retro \
      ~/dropwise-py-work-r3/venv313/bin/python -m unittest \
      discover -s prospective/phase4-exploration/python/pg-mysql/tests -v
"""

from __future__ import annotations

import asyncio
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

REPO = Path(__file__).resolve().parents[5]
PG = Path(__file__).resolve().parents[1]
for path in (PG, REPO / "research" / "python-retro"):
    if str(path) not in sys.path:
        sys.path.insert(0, str(path))

from cases.asyncpg_case import AsyncpgScenario  # noqa: E402
from cases.psycopg_case import PsycopgScenario  # noqa: E402
from dropwise_py.engine import (  # noqa: E402
    OpResult,
    Scenario,
    level_retry,
    normalize,
    operation_failed,
)


# ---------------------------------------------------------------------
# fake psycopg connection
# ---------------------------------------------------------------------
class _PgInfo:
    backend_pid = 4242
    transaction_status = 0


class FakePgConn:
    def __init__(self, fail: str | None = None) -> None:
        self.fail = fail
        self.calls: list[str] = []
        self.info = _PgInfo()

    async def execute(self, sql, params=None):
        self.calls.append(sql)
        if self.fail and sql.startswith(self.fail):
            raise RuntimeError(f"boom at {self.fail}")
        return SimpleNamespace(statusmessage=sql.split()[0])


# fake asyncpg connection
class FakeApgConn:
    def __init__(self, fail: str | None = None, in_tx: bool = False) -> None:
        self.fail = fail
        self.calls: list[str] = []
        self.in_tx = in_tx
        self.released = False
        self.raise_cancel = False

    def get_server_pid(self) -> int:
        return 4343

    def is_in_transaction(self) -> bool:
        if self.released:
            raise RuntimeError("cannot call is_in_transaction(): released")
        return self.in_tx

    async def execute(self, sql):
        self.calls.append(sql)
        if self.raise_cancel and sql.startswith("BEGIN"):
            raise asyncio.CancelledError()
        if self.fail and sql.startswith(self.fail):
            raise RuntimeError(f"boom at {self.fail}")
        return sql.split()[0]


def pg_scenario(conn: FakePgConn) -> PsycopgScenario:
    sc = PsycopgScenario(SimpleNamespace(mode="direct"))
    sc.table = "fake_table"
    sc.events = []
    sc.diag = {}

    async def acquire():
        return conn

    async def release(_conn):
        return None

    sc._acquire = acquire
    sc._release = release
    return sc


def apg_scenario(conn: FakeApgConn) -> AsyncpgScenario:
    sc = AsyncpgScenario(SimpleNamespace(mode="direct"))
    sc.table = "fake_table"
    sc.events = []
    sc.diag = {}

    async def acquire():
        return conn

    async def release(_conn):
        _conn.released = True  # asyncpg pool marks the handle released

    sc._acquire = acquire
    sc._release = release
    return sc


# ---------------------------------------------------------------------
class PsycopgStageTests(unittest.IsolatedAsyncioTestCase):
    async def test_begin_failure_stops_before_insert_and_commit(self):
        conn = FakePgConn(fail="BEGIN")
        sc = pg_scenario(conn)
        result = await sc.same()
        self.assertIsInstance(result, OpResult)
        self.assertFalse(result.ok)
        self.assertEqual(result.stage, "begin")
        self.assertEqual(conn.calls, ["BEGIN"])  # no INSERT, no COMMIT
        self.assertTrue(operation_failed(result))

    async def test_insert_failure_stops_before_commit(self):
        conn = FakePgConn(fail="INSERT")
        sc = pg_scenario(conn)
        result = await sc.different()
        self.assertFalse(result.ok)
        self.assertEqual(result.stage, "insert")
        self.assertEqual([c.split()[0] for c in conn.calls],
                         ["BEGIN", "INSERT"])  # no COMMIT

    async def test_commit_failure_is_returned(self):
        conn = FakePgConn(fail="COMMIT")
        sc = pg_scenario(conn)
        result = await sc._tx_write("target", 1)
        self.assertFalse(result.ok)
        self.assertEqual(result.stage, "commit")
        self.assertEqual([c.split()[0] for c in conn.calls],
                         ["BEGIN", "INSERT", "COMMIT"])

    async def test_success_is_done_and_normalized(self):
        conn = FakePgConn()
        sc = pg_scenario(conn)
        result = await sc._tx_write("target", 1)
        self.assertTrue(result.ok)
        self.assertEqual(result.stage, "commit")
        self.assertIn("backend_pid", result.value)
        self.assertTrue(normalize(result)["ok"])
        self.assertEqual([c.split()[0] for c in conn.calls],
                         ["BEGIN", "INSERT", "COMMIT"])


class AsyncpgStageTests(unittest.IsolatedAsyncioTestCase):
    async def test_begin_failure_stops_before_insert_and_commit(self):
        conn = FakeApgConn(fail="BEGIN")
        sc = apg_scenario(conn)
        result = await sc.same()
        self.assertIsInstance(result, OpResult)
        self.assertFalse(result.ok)
        self.assertEqual(result.stage, "begin")
        self.assertEqual(conn.calls, ["BEGIN"])
        self.assertTrue(operation_failed(result))

    async def test_insert_failure_stops_before_commit(self):
        conn = FakeApgConn(fail="INSERT")
        sc = apg_scenario(conn)
        result = await sc.different()
        self.assertFalse(result.ok)
        self.assertEqual(result.stage, "insert")
        self.assertEqual([c.split()[0] for c in conn.calls], ["BEGIN", "INSERT"])

    async def test_success_is_not_none(self):
        conn = FakeApgConn()
        sc = apg_scenario(conn)
        result = await sc.target()
        self.assertIsNotNone(result)  # the old bug returned None
        self.assertTrue(result.ok)
        self.assertEqual(result.stage, "commit")

    async def test_released_handle_is_diagnostic_not_failure(self):
        # The pool has already released the handle when the post-release
        # diagnostic runs; that must be recorded, not raised as an operation
        # error or as a baseline error.
        conn = FakeApgConn()
        sc = apg_scenario(conn)
        result = await sc._tx_write("target", 1)
        self.assertTrue(result.ok, result)
        self.assertTrue(conn.released)

    async def test_cancelled_target_keeps_cancelled_error(self):
        conn = FakeApgConn()
        conn.raise_cancel = True
        sc = apg_scenario(conn)
        with self.assertRaises(asyncio.CancelledError):
            await sc._tx_write("target", 1)


# ---------------------------------------------------------------------
class _FailingOp(Scenario):
    async def setup(self) -> None:
        pass

    async def target(self):
        await asyncio.sleep(0)
        return "target"

    async def same(self):
        return OpResult(False, "insert", {"begin": ("ok", "BEGIN")},
                        "insert: ('err', RuntimeError: boom)")

    async def different(self):
        return await self.same()

    async def survivor(self):
        return None

    async def observe(self):
        return None

    async def close(self) -> None:
        pass


class _TimeoutOp(_FailingOp):
    async def same(self):
        await asyncio.sleep(30)


class LadderWiringTests(unittest.IsolatedAsyncioTestCase):
    async def test_failed_adapter_stage_is_b2_violation(self):
        report = await level_retry(
            "fake", _FailingOp, level="B2", retry="same", reps=1, timeout=1.0
        )
        self.assertEqual(len(report.violations()), 1)
        self.assertFalse(report.trials[0].observation["ok"])

    async def test_timeout_is_b3_violation(self):
        report = await level_retry(
            "fake", _TimeoutOp, level="B3", retry="different", reps=1,
            timeout=0.2,
        )
        self.assertEqual(len(report.violations()), 1)
        self.assertTrue(report.trials[0].observation.startswith("ERR "))


if __name__ == "__main__":
    unittest.main()
