"""No-service test of the redis adapter's failure contract (not a redis trial).

Round-3 audit follow-up: after the aiomysql/asyncpg adapters were found
returning dicts/None that hid a failed stage, this test pins the contract the
redis adapter now implements for ``target`` / ``same`` / ``different``:

  * every stage either raises, or the operation returns
    ``dropwise_py.engine.OpResult(ok=...)``;
  * a retry that fails (structured, raised, or timed out) must appear as a
    B2 violation from ``level_retry``;
  * a successful retry must not;
  * a target that raises is reported as an engine baseline error
    (``count_yields`` outcome != completed), never a silent pass;
  * returning ``None`` is *not* detectable by the engine, which is exactly
    why the adapter never does it (negative control, expected 0 violations).

No server, no socket, no redis import: the scenarios are fakes whose target
is ``asyncio.sleep(0.01)`` so the ladder has one cancellation point.
"""

from __future__ import annotations

import asyncio
import sys

from .common import BOUND  # noqa: F401  (puts research/python-retro on sys.path)
from dropwise_py import Scenario, level_b1, level_retry
from dropwise_py.engine import OpResult


class Fake(Scenario):
    def __init__(self, mode: str) -> None:
        self.mode = mode

    async def setup(self) -> None:
        pass

    async def close(self) -> None:
        pass

    async def target(self):
        await asyncio.sleep(0.01)          # exactly one suspension point
        if self.mode == "target-fail":
            raise RuntimeError("deliberate target failure")
        return {"ok": True}

    async def same(self):
        if self.mode == "opresult-fail":
            return OpResult(ok=False, stage="fake-exec", error="deliberate")
        if self.mode == "raise":
            raise RuntimeError("deliberate retry raise")
        if self.mode == "timeout":
            await asyncio.sleep(5)
        if self.mode == "silent-none":
            return None
        return OpResult(ok=True, stage="fake-exec", value={"ok": True})

    async def different(self):
        return await self.same()

    async def survivor(self):
        return {}

    async def observe(self):
        return {}


async def main() -> int:
    failures = 0
    print("=== adapter contract selftest (no service) ===", flush=True)

    expected = {
        # mode: (expected B2 violations, expected baseline errors)
        "opresult-fail": (1, 0),
        "raise": (1, 0),
        "timeout": (1, 0),
        "ok": (0, 0),
        "silent-none": (0, 0),   # engine cannot see None; documented control
        "target-fail": (0, 1),   # reported as baseline error, not violation
    }
    for mode, (want_v, want_be) in expected.items():
        r = await level_retry(
            "selftest", lambda m=mode: Fake(m), level="B2", retry="same",
            reps=1, timeout=0.5,
        )
        got_v, got_be = len(r.violations()), len(r.baseline_errors)
        ok = (got_v, got_be) == (want_v, want_be)
        failures += 0 if ok else 1
        obs = r.trials[0].observation if r.trials else None
        print(
            f"B2 mode={mode:<13} violations={got_v} baseline_errors={got_be} "
            f"yields={r.target_yields} trials={len(r.trials)} "
            f"{'PASS' if ok else 'FAIL'} obs={obs!r}",
            flush=True,
        )

    # B1: a failed uncancelled target invalidates the baseline; it is not
    # a cancellation finding and no trials should be judged.
    r = await level_b1(
        "selftest", lambda: Fake("target-fail"), reps=1, timeout=0.5
    )
    ok = len(r.baseline_errors) >= 1
    failures += 0 if ok else 1
    print(
        f"B1 target-fail violations={len(r.violations())} "
        f"baseline_errors={len(r.baseline_errors)} "
        f"{'PASS' if ok else 'FAIL'} {r.baseline_errors}",
        flush=True,
    )

    # A failed retry must be distinguishable from a successful one by
    # operation_failed() on the *normalised* value (the engine normalises
    # before judging; an OpResult must survive that as a dict with ok=False).
    from dropwise_py.engine import normalize, operation_failed

    failed = normalize(OpResult(ok=False, stage="s", error="x"))
    good = normalize(OpResult(ok=True, stage="s", value={}))
    ok = operation_failed(failed) and not operation_failed(good)
    failures += 0 if ok else 1
    print(
        f"normalize/operation_failed failed={failed!r} good={good!r} "
        f"{'PASS' if ok else 'FAIL'}",
        flush=True,
    )

    print(f"=== {'OK' if failures == 0 else f'{failures} FAILURE(S)'} ===")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
