"""No-service tests for the round-4 transaction-phase harness.

They validate the harness itself, not any driver: the phase recorded at
cancellation time, the double-cancel hook, and that a cancellation is not
converted into a target error.  No database or network is used.
"""

from __future__ import annotations

import asyncio
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from cases.txphase_case import _await_phased
from cases.pool_txphase_case import PoolTx, UnrealizedEntry


class PhaseTaskTest(unittest.IsolatedAsyncioTestCase):
    async def _run(self, body, cut, holder, double=False):
        return await _await_phased(
            body(), cut_after=cut, timeout=2, holder=holder, double=double
        )

    async def test_single_cut_records_enter_and_body(self) -> None:
        for cut, expected in ((1, "enter"), (2, "body")):
            holder = {"phase": "setup"}

            async def body():
                holder["phase"] = "enter"
                await asyncio.sleep(0.01)
                holder["phase"] = "body"
                await asyncio.sleep(0.01)
                return "done"

            tr, meta = await self._run(body, cut, holder)
            self.assertEqual(tr.outcome, "cancelled")
            self.assertTrue(tr.realized)
            self.assertEqual(meta["cut_phase"], expected)
            self.assertEqual(meta["second_phase"], None)

    async def test_double_cancel_records_cleanup_phase(self) -> None:
        holder = {"phase": "setup"}

        async def body():
            holder["phase"] = "body"
            try:
                await asyncio.sleep(0.01)
            finally:
                holder["phase"] = "cleanup"
                await asyncio.sleep(0.02)

        tr, meta = await self._run(body, 1, holder, double=True)
        self.assertEqual(tr.outcome, "cancelled")
        self.assertTrue(tr.realized)
        self.assertEqual(meta["cut_phase"], "body")
        self.assertEqual(meta["second_phase"], "cleanup")
        self.assertTrue(meta["second_requested"])

    async def test_clean_completion_is_not_a_cut(self) -> None:
        holder = {"phase": "setup"}

        async def body():
            holder["phase"] = "body"
            return "ok"

        tr, meta = await self._run(body, 1, holder)
        self.assertEqual(tr.outcome, "completed")
        self.assertFalse(tr.cut_requested)
        self.assertEqual(tr.result, "ok")

    async def test_pool_entry_uses_context_cleanup_for_both_drivers(self) -> None:
        class Connection:
            def __init__(self):
                self.exits = []

            def transaction(self):
                return self

            async def __aenter__(self):
                return self

            async def __aexit__(self, typ, exc, tb):
                self.exits.append(typ)

        for driver in ("psycopg", "asyncpg"):
            conn = Connection()
            holder = {}
            sc = PoolTx(driver, "127.0.0.1", 1, 2)
            with patch("cases.pool_txphase_case.asyncio.sleep", return_value=None):
                await sc._entry(conn, holder)
            self.assertEqual(holder["phase"], "body")
            self.assertEqual(conn.exits, [None])

            conn = Connection()
            holder = {}
            tr, meta = await _await_phased(
                sc._entry(conn, holder), cut_after=1, timeout=2,
                holder=holder, double=False,
            )
            self.assertEqual(tr.outcome, "cancelled")
            self.assertEqual(meta["cut_phase"], "body")
            self.assertEqual(conn.exits, [asyncio.CancelledError])

    async def test_pool_entry_cut_does_not_call_exit(self) -> None:
        class Connection:
            def transaction(self):
                return self

            async def __aenter__(self):
                await asyncio.sleep(0.01)

            async def __aexit__(self, *args):
                raise AssertionError("entry failed: exit must not run")

        for driver in ("psycopg", "asyncpg"):
            holder = {}
            sc = PoolTx(driver, "127.0.0.1", 1, 2)
            tr, meta = await _await_phased(
                sc._entry(Connection(), holder), cut_after=1, timeout=2,
                holder=holder, double=False,
            )
            self.assertTrue(tr.realized)
            self.assertEqual(tr.outcome, "cancelled")
            self.assertEqual(meta["cut_phase"], "enter")

    async def test_unrealized_entry_is_a_trial_failure(self) -> None:
        sc = PoolTx("asyncpg", "127.0.0.1", 1, 2)
        sc.observer = SimpleNamespace(execute=AsyncMock())
        with patch.object(sc, "_acquire_block", side_effect=UnrealizedEntry("wrong phase")):
            with self.assertRaises(UnrealizedEntry):
                await sc.one("propagate", 1)


if __name__ == "__main__":
    unittest.main()
