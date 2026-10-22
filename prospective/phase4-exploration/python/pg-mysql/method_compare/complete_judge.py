"""The original calibrated successor differential on the same target.

This is the complete method, not an ablation: it samples a no-target pre
observation and a completed-target post observation, then runs the survivor and
the independent observer after each cut.  A trial is Pre/Post/Mixed/Neither;
Neither is an Atomic violation.

It is intentionally kept separate from ``effect_judge.py`` so the comparison
does not weaken the baseline or attribute the ablation's labels to the
complete method.
"""
from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import sys
from types import SimpleNamespace

from cases.common import add_db_args
from cases.txphase_case import CmScenario
from dropwise_py.engine import Expect, explore_observed
from successor_probe.run_pg import package_info


def factory(args) -> CmScenario:
    return CmScenario(SimpleNamespace(
        host=args.host, port=args.port, user=args.user,
        password=args.password, dbname=args.dbname,
        driver=args.driver, recovery="none",
    ))


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--driver", default="psycopg", choices=["psycopg", "asyncpg"])
    ap.add_argument("--repair", choices=["none", "actual"], default="none",
                    help="'actual' requires the loaded package repair marker")
    ap.add_argument("--cuts", default="", help="comma-separated k; empty = all")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    info = package_info(args.driver)
    marker = ("has_recover_entry_failure" if args.driver == "psycopg"
              else "start_has_top_xact_clear")
    if args.repair == "actual" and not info.get(marker):
        raise SystemExit(
            f"--repair actual requested but loaded package lacks {marker}: {info}"
        )
    plans = ([int(x) for x in args.cuts.split(",") if x.strip()]
             if args.cuts.strip() else None)
    report = await explore_observed(
        f"cm-{args.driver}", lambda: factory(args), expect=Expect.ATOMIC,
        reps=args.reps, timeout=15.0, plans=plans,
    )
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    try:
        print("COMPLETE-CALIBRATION " + json.dumps({
            "driver": args.driver, "repair": args.repair,
            "package": info, "target_yields": report.target_yields,
            "pre": report.pre, "post": report.post,
            "calibration_ok": (len(report.pre) == 2 and report.pre[0] == report.pre[1]
                               and len(report.post) == 2
                               and report.post[0] == report.post[1]),
            "notes": report.notes, "exhaustive": report.exhaustive,
            "baseline_errors": report.baseline_errors,
        }, default=str, sort_keys=True), file=out, flush=True)
        for trial in report.trials:
            rec = {
                "driver": args.driver, "plan": trial.plan, "rep": trial.rep,
                "verdict": trial.verdict.value if trial.verdict else None,
                "violation": trial.violation,
                "target": None if trial.target is None else {
                    "outcome": trial.target.outcome,
                    "yields": trial.target.yields,
                    "realized": trial.target.realized,
                    "error": trial.target.error,
                },
                "observation": trial.observation,
                "seconds": round(trial.seconds, 3),
            }
            print("COMPLETE " + json.dumps(rec, default=str, sort_keys=True),
                  file=out, flush=True)
        print("COMPLETE-TOTAL " + json.dumps({
            "driver": args.driver, "repair": args.repair,
            "trials": len(report.trials),
            "verdicts": report.verdict_counts(),
            "violations": len(report.violations()),
            "unknown": len(report.unknown()),
            "seconds": round(report.seconds, 2),
        }, sort_keys=True), file=out, flush=True)
    finally:
        if out is not sys.stdout:
            out.close()
    return 0


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
