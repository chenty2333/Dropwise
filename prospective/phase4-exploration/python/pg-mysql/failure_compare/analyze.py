"""Aggregate one `failure_compare` run into a compact JSON/markdown summary.

Usage (from .../python/pg-mysql):
    python failure_compare/analyze.py failure_compare/logs/run.fc.<stamp>

Reads trials.jsonl (and calibration files if present), groups by driver + arm,
and reports only the fields the comparison uses.  It does not judge a "winner";
counts of realized/unrealized and silent-success are kept separate.
"""

from __future__ import annotations

import argparse
import collections
import json
import os


def state_signature(rec: dict) -> str:
    c = rec.get("client_after_target") or {}
    if c.get("kind") == "psycopg":
        return f"psycopg:{c.get('status')}/n={c.get('num_transactions')}"
    if c.get("kind") == "asyncpg":
        extra = f"/top={c.get('top_xact_state')}" if c.get("top_xact_set") else ""
        return (f"asyncpg:in_tx={c.get('in_transaction')}"
                f"/tx={c.get('tx_state')}{extra}")
    if rec.get("detail"):
        d = rec["detail"]
        c = d.get("client_after_outer_exit") or d.get("client_after_failure") or {}
        if c.get("kind") == "psycopg":
            return f"psycopg:{c.get('status')}/n={c.get('num_transactions')}"
        return (f"asyncpg:in_tx={c.get('in_transaction')}"
                f"/tx={c.get('tx_state')}")
    return "unavailable"


def recovery_facts(rec: dict) -> dict:
    r = rec.get("recovery") or {}
    out = {}
    for key in ("rollback", "raw_rollback", "reset"):
        if key in r:
            val = r[key]
            if isinstance(val, list) and val and val[0] == "err":
                out[key] = str(val[1])[:80]
            elif isinstance(val, list):
                out[key] = val[0]
    retry = r.get("retry_after_raw")
    if retry:
        out["retry_after_raw"] = (
            "ok" if retry.get("ok") else
            ((retry.get("exc") or {}).get("class") or "error")
        )
    return out


def level(rec: dict) -> str:
    d = rec.get("detail") or {}
    if d.get("nested_enter_exc"):
        return f"nested_entry={(d['nested_enter_exc'].get('class'))}/" \
               f"{d['nested_enter_exc'].get('sqlstate')}"
    if d.get("entry_exc"):
        return f"entry={d['entry_exc'].get('class')}/" \
               f"{d['entry_exc'].get('sqlstate')}"
    if d.get("outer_exit_exc"):
        return f"outer_exit={d['outer_exit_exc'].get('class')}"
    if d.get("outer_exit"):
        return f"outer_exit={d['outer_exit']}"
    t = rec.get("target") or {}
    e = t.get("exc") or {}
    if e:
        return f"{e.get('class')}/{e.get('sqlstate')}"
    return t.get("outcome") or "?"


def summarize(run_dir: str) -> dict:
    path = os.path.join(run_dir, "trials.jsonl")
    rows = [json.loads(line) for line in open(path, encoding="utf-8")]
    groups: dict[tuple[str, str], list[dict]] = collections.defaultdict(list)
    for r in rows:
        groups[(r.get("driver"), r.get("arm"))].append(r)

    out = {"run": run_dir, "trials": len(rows),
           "harness_errors": sum("harness_error" in r for r in rows),
           "unrealized": sum(bool(r.get("unrealized")) for r in rows),
           "groups": {}}
    for (driver, arm), recs in sorted(groups.items()):
        n = len(recs)
        g = {
            "n": n,
            "realized": sum(bool(r.get("realized")) for r in recs),
            "unrealized": sum(bool(r.get("unrealized")) for r in recs),
            "states": dict(collections.Counter(state_signature(r) for r in recs)),
            "successor_ok": sum(r.get("successor_ok") is True for r in recs),
            "successor_failed": sum(r.get("successor_ok") is False for r in recs),
            "retry_visible": sum(bool(r.get("retry_visible")) for r in recs),
            "silent_successor": sum(bool(r.get("successor_silent_success"))
                                    for r in recs),
            "target_visible_after_target": sum(
                bool(r.get("target_visible_after_target")) for r in recs),
            "target_silent_success": sum(bool(r.get("target_silent_success"))
                                         for r in recs),
            "silent_exit_after_handled_error": sum(
                bool(r.get("silent_exit_after_handled_error")) for r in recs),
            "levels": dict(collections.Counter(level(r) for r in recs)),
            "recovery": [recovery_facts(r) for r in recs],
            "cleanup": dict(collections.Counter(
                r.get("cleanup_obligation") for r in recs)),
            "seconds": {
                "sum": round(sum(r.get("seconds") or 0 for r in recs), 2),
                "min": min((r.get("seconds") or 0) for r in recs),
                "max": max((r.get("seconds") or 0) for r in recs),
            },
        }
        out["groups"][f"{driver}/{arm}"] = g
    calib = {}
    for name in ("psycopg", "asyncpg"):
        p = os.path.join(run_dir, f"calibration-{name}.json")
        if os.path.exists(p):
            calib[name] = json.load(open(p, encoding="utf-8"))
    out["calibration"] = calib
    meta_path = os.path.join(run_dir, "meta.json")
    if os.path.exists(meta_path):
        meta = json.load(open(meta_path, encoding="utf-8"))
        out["started"] = meta.get("started")
        out["finished"] = meta.get("finished")
        out["delay"] = meta.get("delay")
        out["settle"] = meta.get("settle")
        out["reps"] = meta.get("reps")
        out["server"] = meta.get("server")
    return out


def markdown(summary: dict) -> str:
    lines = [
        f"trials={summary['trials']} harness_errors={summary['harness_errors']} "
        f"unrealized={summary['unrealized']}",
        "",
        "| driver/arm | n | realized | client state | error (class/sqlstate) | "
        "successor ok/fail | retry visible | silent successor | target visible |",
        "|---|---:|---:|---|---|---|---:|---:|---:|",
    ]
    for name, g in summary["groups"].items():
        states = "<br>".join(f"{k} x{v}" for k, v in sorted(g["states"].items()))
        levels = "<br>".join(f"{k} x{v}" for k, v in sorted(g["levels"].items()))
        lines.append(
            f"| {name} | {g['n']} | {g['realized']} | {states} | {levels} | "
            f"{g['successor_ok']}/{g['successor_failed']} | "
            f"{g['retry_visible']} | {g['silent_successor']} | "
            f"{g['target_visible_after_target']} |"
        )
    return "\n".join(lines)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("run_dir")
    p.add_argument("--json", default=None,
                   help="write summary JSON here (default: <run>/summary.json)")
    args = p.parse_args()
    summary = summarize(args.run_dir)
    out_json = args.json or os.path.join(args.run_dir, "summary.json")
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(summary, fh, indent=2, sort_keys=True)
    text = markdown(summary)
    with open(os.path.join(args.run_dir, "summary.md"), "w", encoding="utf-8") as fh:
        fh.write(text + "\n")
    print(text)
    print(json.dumps({k: summary[k] for k in
                      ("trials", "harness_errors", "unrealized",
                       "started", "finished")}, sort_keys=True))


if __name__ == "__main__":
    main()
