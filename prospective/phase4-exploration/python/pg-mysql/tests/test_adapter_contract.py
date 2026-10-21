"""No-service regression tests for the Python ladder's retry contract.

Run with the round-3 venv:

    PYTHONPATH=../../../../research/python-retro \
      ~/dropwise-py-work-r3/venv313/bin/python tests/test_adapter_contract.py

These use a fake Scenario (no database, no proxy) to prove that
`level_retry` treats all of the following as a B2/B3 violation:

  * `same()` returning `OpResult(ok=False, ...)` (the new structured contract),
  * `same()` raising an ordinary exception,
  * `same()` raising `asyncio.TimeoutError`.

and that a successful `OpResult(ok=True, ...)` is not a violation.  This
protects the aiomysql/mysqlconnector adapters from the old false-negative where
a swallowed per-stage error was reported as `ok`.
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path

HERE = Path(__file__).resolve()
REPO = HERE.parents[5]
RETRO = REPO / "research" / "python-retro"
if str(RETRO) not in sys.path:
    sys.path.insert(0, str(RETRO))

from dropwise_py import Scenario, level_retry  # noqa: E402
from dropwise_py.engine import OpResult  # noqa: E402


class FakeScenario(Scenario):
    """One target suspension; `same()` behaviour is injected per test."""

    def __init__(self, same_impl) -> None:
        self._same_impl = same_impl

    async def setup(self) -> None:
        return None

    async def target(self):
        await asyncio.sleep(0)
        return OpResult(True, "target")

    async def same(self):
        return await self._same_impl()

    async def different(self):
        return await self._same_impl()

    async def survivor(self):
        return {"ok": True}

    async def observe(self):
        return {"rows": []}

    async def close(self) -> None:
        return None


async def run_level(same_impl) -> object:
    factory = lambda: FakeScenario(same_impl)
    return await level_retry(
        "fake", factory, level="B2", retry="same", reps=1, timeout=2.0, plans=[1]
    )


async def check_structured_failure() -> None:
    async def same():
        return OpResult(False, "insert", value={"begin": ("ok", 0)}, error="err: boom")

    report = await run_level(same)
    assert report.target_yields == 1, report.target_yields
    assert len(report.violations()) == 1, report.trials
    obs = report.trials[0].observation
    assert isinstance(obs, dict) and obs["ok"] is False and obs["stage"] == "insert", obs
    print("ok OpResult(ok=False) -> violation:", obs)


async def check_raised_failure() -> None:
    async def same():
        raise RuntimeError("adapter raised")

    report = await run_level(same)
    assert len(report.violations()) == 1, report.trials
    obs = report.trials[0].observation
    assert isinstance(obs, str) and obs.startswith("ERR RuntimeError"), obs
    print("ok raised RuntimeError -> violation:", obs)


async def check_timeout() -> None:
    async def same():
        raise asyncio.TimeoutError("bounded operation timed out")

    report = await run_level(same)
    assert len(report.violations()) == 1, report.trials
    obs = report.trials[0].observation
    assert isinstance(obs, str) and obs.startswith("ERR TimeoutError"), obs
    print("ok raised TimeoutError -> violation:", obs)


async def check_structured_timeout() -> None:
    async def same():
        return OpResult(False, "commit", error="timeout: 2.0s")

    report = await run_level(same)
    assert len(report.violations()) == 1, report.trials
    print("ok OpResult(timeout) -> violation:", report.trials[0].observation)


async def check_success() -> None:
    async def same():
        return OpResult(True, "commit", value={"begin": ("ok", 0)})

    report = await run_level(same)
    assert len(report.violations()) == 0, report.trials
    print("ok OpResult(ok=True) -> no violation:", report.trials[0].observation)


async def main() -> None:
    await check_structured_failure()
    await check_raised_failure()
    await check_timeout()
    await check_structured_timeout()
    await check_success()
    print("ALL ADAPTER-CONTRACT TESTS PASSED")


if __name__ == "__main__":
    asyncio.run(main())
