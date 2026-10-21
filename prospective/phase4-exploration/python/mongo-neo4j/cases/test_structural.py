"""No-service structural test for the retry/survivor adapter contract.

A failed `same()`/`different()` and a timed-out retry must be reported as
violations; a clean retry must not.  This test uses a fake scenario and never
touches MongoDB, Neo4j, the proxy or the network.  It exists because the
aiomysql/asyncpg audit found adapters that returned a plain dict/None after a
failed stage, which the old string-prefix check counted as success.

Run from this directory:

    PYTHONPATH=".:<repo>/research/python-retro" \
      python -m cases.test_structural
"""

from __future__ import annotations

import asyncio
import sys

from . import common  # noqa: F401  (puts research/python-retro on sys.path)

from dropwise_py import level_b1, level_retry  # noqa: E402
from dropwise_py.engine import (  # noqa: E402
    OpResult,
    Scenario,
    operation_failed,
)


class FakeScenario(Scenario):
    """One-yield target; retry behaviour depends on `mode`."""

    def __init__(self, mode: str) -> None:
        self.mode = mode

    async def setup(self) -> None:
        return None

    async def target(self):
        if self.mode == "target_raises":
            # Raise before the first suspension: invalidate the baseline.
            raise RuntimeError("target boom")
        await asyncio.sleep(0)  # exactly one task yield
        return OpResult(True, "target", value={"v": 1})

    async def same(self):
        if self.mode == "same_fails":
            return OpResult(False, "same.write", value={"v": None},
                            error="write failed")
        if self.mode == "same_timeout":
            await asyncio.sleep(5)
        return OpResult(True, "same", value={"v": 2})

    async def different(self):
        return await self.same()

    async def survivor(self):
        return OpResult(True, "survivor", value={"v": 3})

    async def observe(self):
        return OpResult(True, "observer", value={"v": 4})

    async def close(self) -> None:
        return None


def factory(mode: str):
    return lambda: FakeScenario(mode)


async def main() -> int:
    checks: list[tuple[str, bool]] = []

    # The structured predicate itself.
    checks.append(("operation_failed(OpResult(ok=False))",
                   operation_failed(OpResult(False, "x", error="boom"))))
    checks.append(("operation_failed({ok: False})",
                   operation_failed({"ok": False, "stage": "x"})))
    checks.append(("not operation_failed(OpResult(ok=True))",
                   not operation_failed(OpResult(True, "x"))))

    r = await level_retry("fake-fail", factory("same_fails"), level="B2",
                          retry="same", reps=1, timeout=2.0, plans=[1])
    checks.append(("failed same() is a violation", len(r.violations()) == 1))

    r = await level_retry("fake-timeout", factory("same_timeout"), level="B3",
                          retry="different", reps=1, timeout=0.3, plans=[1])
    checks.append(("timed-out retry is a violation", len(r.violations()) == 1))

    r = await level_b1("fake-target", factory("target_raises"), reps=1,
                       timeout=2.0, plans=[1])
    checks.append(("raising target invalidates baseline, not a cancellation finding",
                   bool(r.baseline_errors) and not r.trials and not r.exhaustive))

    r = await level_retry("fake-clean", factory("clean"), level="B2",
                          retry="same", reps=1, timeout=2.0, plans=[1])
    checks.append(("clean retry has no violation", not r.violations()))

    for name, ok in checks:
        print(("PASS " if ok else "FAIL ") + name)
    bad = [name for name, ok in checks if not ok]
    print(f"structural-test failures={len(bad)}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
