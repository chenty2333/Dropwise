"""Measure the cut scale of representative targets, not just one K.

For each target this script records:

* K (the number of observed Pending/yield boundaries on the normal path) over
  repeated runs;
* the lexical ``await`` expressions in the harness target functions, with
  their source lines, separately from K;
* the actual suspension location (coroutine chain) at every normal-path yield;
* one trial at each k in 1..K plus one overrun request K+1;
* the scenario-declared phase at the cut (entry/body/exit/handoff);
* wall time for setup, count runs, cut trials and total.

The overrun request is not a failure: a target that completes before the
requested suspension has no such cut.  It is kept separate from an unrealised
cut inside the observed range and from a source await that is not a Pending
boundary.  A single trace is not an exhaustive schedule; repetitions are
reported as count variation, not as scheduling coverage.
"""

from __future__ import annotations

import argparse
import ast
import asyncio
import contextlib
import inspect
import json
import sys
import textwrap
import time
from pathlib import Path
from types import SimpleNamespace

from asyncio.tasks import _PyTask as PureTask

from cases.common import add_db_args, close_quiet, err_text
from cases.txphase_case import CmScenario
from dropwise_py.engine import OpResult, TargetRun

from successor_probe.run_pool import PoolProbeScenario


class _LocationCutTask(PureTask):
    def __init__(self, coro, *, cut_after: int | None, holder: dict) -> None:
        self.yields = 0
        self.cut_after = cut_after
        self.holder = holder
        self.cut_requested = False
        self.cut_phase: str | None = None
        self.locations: list[dict] = []
        super().__init__(coro)

    def _Task__step(self, exc=None) -> None:  # name-mangled override
        super()._Task__step(exc)
        if self.done():
            return
        self.yields += 1
        loc = self._locations()
        self.locations.append({"yield": self.yields, "phase": self.holder.get("phase"),
                               "chain": loc})
        if (
            self.cut_after is not None
            and not self.cut_requested
            and self.yields == self.cut_after
        ):
            self.cut_requested = True
            self.cut_phase = self.holder.get("phase")
            self.cancel()

    def _locations(self) -> list[dict]:
        out: list[dict] = []
        coro = self.get_coro()
        seen = 0
        while coro is not None and seen < 12:
            frame = getattr(coro, "cr_frame", None)
            if frame is not None:
                out.append({
                    "file": Path(frame.f_code.co_filename).name,
                    "line": frame.f_lineno,
                    "func": frame.f_code.co_name,
                })
            coro = getattr(coro, "cr_await", None)
            seen += 1
        return out


async def _await_location(coro, cut_after: int | None, timeout: float,
                          holder: dict) -> tuple[TargetRun, dict]:
    started = time.monotonic()
    task = _LocationCutTask(coro, cut_after=cut_after, holder=holder)
    outcome = "error"
    result = None
    error = None
    try:
        result = await asyncio.wait_for(task, timeout)
        if isinstance(result, OpResult) and not result.ok:
            outcome = "error"
            error = f"target reported failure: {result!r}"
        else:
            outcome = "completed"
    except asyncio.CancelledError:
        outcome = "cancelled"
    except asyncio.TimeoutError:
        outcome = "timeout"
        error = f"timeout after {timeout:.1f}s"
        task.cancel()
        with contextlib.suppress(BaseException):
            await task
    except BaseException as exc:
        outcome = "error"
        error = err_text(exc)
    run = TargetRun(
        outcome=outcome, result=result, error=error, yields=task.yields,
        cut_requested=task.cut_requested, cut_delivered=task.cut_requested,
        seconds=time.monotonic() - started,
    )
    meta = {
        "cut_phase": task.cut_phase,
        "locations": task.locations,
        "cut_location": next(
            (x for x in task.locations if x["yield"] == cut_after), None
        ) if cut_after is not None else None,
    }
    return run, meta


class NestedScenario(CmScenario):
    """A more complex operation: outer transaction + inner savepoint + writes."""

    async def target(self):
        self.cut_meta = None
        out: dict = {}
        self.holder["phase"] = "outer_enter"
        async with self.conn.transaction():
            self.holder["phase"] = "outer_body"
            out["outer"] = await self._execute_upsert("outer", 1)
            self.holder["phase"] = "inner_enter"
            async with self.conn.transaction():
                self.holder["phase"] = "inner_body"
                out["inner"] = await self._execute_upsert("inner", 2)
            self.holder["phase"] = "outer_exit"
        self.holder["phase"] = "done"
        return OpResult(True, "nested", out)


class PoolTargetScenario:
    """Pool checkout wrapped as the target, so K includes handoff yields."""

    def __init__(self, args, tag: str) -> None:
        self.args = args
        self.sc = PoolProbeScenario(args, tag=tag)
        self.holder = {"phase": "setup"}
        self.sc.holder = self.holder

    async def setup(self):
        await self.sc.setup()

    async def close(self):
        await self.sc.close()

    async def target(self):
        self.holder["phase"] = "pool_checkout"
        async with self.sc._checkout() as conn:
            self.holder["phase"] = "tx_enter"
            await self.sc._tx_target(conn)
        self.holder["phase"] = "released"


def _source_awaits(functions: list[tuple[str, object]]) -> list[dict]:
    out: list[dict] = []
    for label, fn in functions:
        try:
            src = textwrap.dedent(inspect.getsource(fn))
            start = inspect.getsourcelines(fn)[1]
            tree = ast.parse(src)
        except (OSError, TypeError, SyntaxError) as exc:
            out.append({"function": label, "error": err_text(exc)})
            continue
        for node in ast.walk(tree):
            if isinstance(node, ast.Await):
                out.append({
                    "function": label,
                    "line": start + node.lineno - 1,
                    "col": node.col_offset,
                    "expr": ast.unparse(node.value)[:160],
                })
    return out


def _observed_source_lines(awaits: list[dict], locations: list[dict]) -> list[dict]:
    for item in awaits:
        if "line" not in item:
            continue
        item["observed_in_first_trace"] = False
        for loc in locations:
            for frame in loc["chain"]:
                if frame["line"] == item["line"]:
                    item["observed_in_first_trace"] = True
                    item["first_yield"] = loc["yield"]
                    break
            if item["observed_in_first_trace"]:
                break
    return awaits


async def measure_target(name: str, factory, functions, args) -> dict:
    started = time.monotonic()
    runs = []
    count_seconds = 0.0
    first_meta = None
    for _ in range(args.count_runs):
        sc = factory()
        setup_started = time.monotonic()
        await asyncio.wait_for(sc.setup(), 25)
        setup_seconds = time.monotonic() - setup_started
        try:
            tr, meta = await _await_location(
                sc.target(), None, args.timeout, getattr(sc, "holder", {})
            )
            if first_meta is None:
                first_meta = meta
            runs.append({
                "outcome": tr.outcome, "yields": tr.yields,
                "seconds": round(tr.seconds, 4),
                "setup_seconds": round(setup_seconds, 4),
                "error": tr.error,
            })
            count_seconds += setup_seconds + tr.seconds
        finally:
            await close_quiet(sc.close())
    k = max((r["yields"] for r in runs), default=0)
    variations = sorted({r["yields"] for r in runs})

    cut_trials = []
    cut_seconds = 0.0
    for kk in list(range(1, k + 1)) + [k + 1]:
        sc = factory()
        setup_started = time.monotonic()
        await asyncio.wait_for(sc.setup(), 25)
        setup_seconds = time.monotonic() - setup_started
        try:
            tr, meta = await _await_location(
                sc.target(), kk, args.timeout, getattr(sc, "holder", {})
            )
            cut_trials.append({
                "k": kk,
                "requested": True,
                "realized": tr.realized,
                "outcome": tr.outcome,
                "phase": meta["cut_phase"],
                "cut_location": meta["cut_location"],
                "yields_total": tr.yields,
                "seconds": round(tr.seconds, 4),
                "setup_seconds": round(setup_seconds, 4),
                "error": tr.error,
            })
            cut_seconds += setup_seconds + tr.seconds
        finally:
            await close_quiet(sc.close())
    awaits = _observed_source_lines(_source_awaits(functions), first_meta["locations"] if first_meta else [])
    return {
        "target": name,
        "K": k,
        "K_values": [r["yields"] for r in runs],
        "K_variations": variations,
        "normal_runs": runs,
        "source_awaits": awaits,
        "source_await_count": len([a for a in awaits if "line" in a]),
        "first_trace_locations": first_meta["locations"] if first_meta else [],
        "cut_trials": cut_trials,
        "realized_cuts": [t["k"] for t in cut_trials if t["realized"]],
        "unrealized_requested": [t["k"] for t in cut_trials if not t["realized"]],
        "phases": {str(t["k"]): t["phase"] for t in cut_trials},
        "seconds": {
            "count_runs": round(count_seconds, 2),
            "cut_trials": round(cut_seconds, 2),
            "total": round(time.monotonic() - started, 2),
        },
    }


def build_targets(args):
    def psycopg_direct():
        return CmScenario(SimpleNamespace(
            host=args.host, port=args.port, user=args.user,
            password=args.password, dbname=args.dbname, driver="psycopg",
            recovery="none",
        ))

    def asyncpg_direct():
        return CmScenario(SimpleNamespace(
            host=args.host, port=args.port, user=args.user,
            password=args.password, dbname=args.dbname, driver="asyncpg",
            recovery="none",
        ))

    def nested():
        return NestedScenario(SimpleNamespace(
            host=args.host, port=args.port, user=args.user,
            password=args.password, dbname=args.dbname, driver="psycopg",
            recovery="none",
        ))

    def pooled():
        return PoolTargetScenario(args, tag=f"cutscale-{time.time_ns()}")

    targets = {
        "direct-psycopg": (
            psycopg_direct,
            [("CmScenario.target", CmScenario.target),
             ("CmScenario._cm_write", CmScenario._cm_write)],
        ),
        "direct-asyncpg": (
            asyncpg_direct,
            [("CmScenario.target", CmScenario.target),
             ("CmScenario._cm_write", CmScenario._cm_write)],
        ),
        "nested-psycopg": (
            nested,
            [("NestedScenario.target", NestedScenario.target),
             ("CmScenario._execute_upsert", CmScenario._execute_upsert)],
        ),
        "pool-checkout-psycopg": (
            pooled,
            [("PoolProbeScenario._tx_target", PoolProbeScenario._tx_target)],
        ),
    }
    return targets


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--targets", default="direct-psycopg,direct-asyncpg,nested-psycopg,pool-checkout-psycopg")
    ap.add_argument("--count-runs", type=int, default=5)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    if not hasattr(args, "driver"):
        args.driver = "psycopg"
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    all_targets = build_targets(args)
    results = []
    failures = 0
    try:
        for name in args.targets.split(","):
            name = name.strip()
            if name not in all_targets:
                raise SystemExit(f"unknown target {name!r}")
            factory, functions = all_targets[name]
            try:
                rec = await measure_target(name, factory, functions, args)
            except asyncio.CancelledError:
                raise
            except BaseException as exc:
                failures += 1
                rec = {"target": name, "error": err_text(exc),
                       "trace": __import__("traceback").format_exc()[-1600:]}
            results.append(rec)
            print("CUTSCALE " + json.dumps(rec, default=str, sort_keys=True),
                  file=out, flush=True)
            if "error" in rec:
                print(f"CUTSCALE-SUMMARY target={name} ERROR={rec['error']}",
                      flush=True)
            else:
                print(
                    f"CUTSCALE-SUMMARY target={name} K={rec['K']} "
                    f"variations={rec['K_variations']} "
                    f"source_awaits={rec['source_await_count']} "
                    f"realized={len(rec['realized_cuts'])}/{len(rec['cut_trials'])} "
                    f"phases={rec['phases']} seconds={rec['seconds']}",
                    flush=True,
                )
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
