"""Effect-only differential judgment, for comparison with cutpoint/probe methods.

It asks only: did the target row become visible, and did the target report
completion?  It does not inspect the handle or the server session after the cut.
That is the point: compare discovery and false positives against the handle
probe, without weakening either method.
"""
from __future__ import annotations
import argparse, asyncio, contextlib, json, sys, time
from types import SimpleNamespace
from cases.common import add_db_args, close_quiet, err_text
from cases.txphase_case import CmScenario, _await_phased
from successor_probe.run_pg import package_info

async def one(args, driver: str, k: int) -> dict:
    sc = CmScenario(SimpleNamespace(
        host=args.host, port=args.port, user=args.user, password=args.password,
        dbname=args.dbname, driver=driver, recovery="none"))
    t0 = time.monotonic()
    try:
        await asyncio.wait_for(sc.setup(), 15)
        tr, meta = await _await_phased(
            sc.target(), cut_after=k, timeout=12, holder=sc.holder, double=False)
        rows = await sc._observer_rows()
        target_visible = ["target", 1] in rows
        if tr.outcome == "completed":
            verdict = "clean_completed" if target_visible else "missing_effect"
        elif tr.outcome == "cancelled":
            verdict = "visible_after_cancel" if target_visible else "no_effect"
        else:
            verdict = "unknown_" + tr.outcome
        return {"driver": driver, "k": k, "phase": meta["cut_phase"],
                "outcome": tr.outcome, "rows": rows, "verdict": verdict,
                "seconds": round(time.monotonic() - t0, 3)}
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        return {"driver": driver, "k": k, "verdict": "unknown_error",
                "error": err_text(exc), "seconds": round(time.monotonic() - t0, 3)}
    finally:
        await close_quiet(sc.close())

async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--driver", default="psycopg", choices=["psycopg", "asyncpg"])
    ap.add_argument("--repair", choices=["none", "actual"], default="none")
    ap.add_argument("--cuts", default="1,2,3,4,5,6")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    info = package_info(args.driver)
    marker = ("has_recover_entry_failure" if args.driver == "psycopg"
              else "start_has_top_xact_clear")
    if args.repair == "actual" and not info.get(marker):
        raise SystemExit(
            f"--repair actual requested but loaded package lacks {marker}: {info}"
        )
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    recs = []
    try:
        for rep in range(1, args.reps + 1):
            for k in [int(x) for x in args.cuts.split(",") if x.strip()]:
                rec = await one(args, args.driver, k)
                rec["rep"] = rep
                rec["repair"] = args.repair
                recs.append(rec)
                print("EFFECT " + json.dumps(rec, default=str, sort_keys=True), file=out,
                      flush=True)
                print(f"EFFECT-SUMMARY driver={args.driver} k={k} rep={rep} "
                      f"phase={rec.get('phase')} outcome={rec.get('outcome')} "
                      f"rows={rec.get('rows')} verdict={rec['verdict']} "
                      f"seconds={rec['seconds']}", flush=True)
        print("EFFECT-TOTAL " + json.dumps({
            "driver": args.driver, "repair": args.repair, "package": info,
            "trials": len(recs),
            "verdicts": {v: sum(1 for r in recs if r["verdict"] == v)
                         for v in sorted({r["verdict"] for r in recs})},
            "seconds": round(sum(r["seconds"] for r in recs), 2)},
            sort_keys=True), file=out, flush=True)
    finally:
        if out is not sys.stdout:
            out.close()
    return 0

if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
