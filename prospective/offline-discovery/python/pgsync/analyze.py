"""Turn the raw JSONL experiment logs into compact tables.

Usage: python -m pgsync.analyze --dir /tmp/dwod/logs
Reads phases2.jsonl, fix2.jsonl, sweep*.jsonl, pool.jsonl, sigint.jsonl,
contrast.jsonl, fields.jsonl and prints markdown to stdout.
"""

from __future__ import annotations

import argparse
import collections
import glob
import json
import os


def records(pattern):
    for fn in sorted(glob.glob(pattern)):
        for line in open(fn):
            line = line.strip()
            if line.startswith("{"):
                try:
                    yield fn, json.loads(line)
                except json.JSONDecodeError:
                    pass


def load(fn):
    if not os.path.exists(fn):
        return []
    out = []
    for line in open(fn):
        line = line.strip()
        if line.startswith("{"):
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def phase_table(fn, title):
    recs = load(fn)
    trials = [r for r in recs if r.get("kind") == "trial"]
    calib = [r for r in recs if r.get("kind") == "calibration"]
    groups = collections.defaultdict(list)
    for r in trials:
        groups[r["key"]].append(r)
    print(f"\n### {title}\n")
    print("| phase | mode | n | verdicts | state after interrupt |")
    print("|---|---|---|---|---|")
    for key in sorted(groups):
        phase, mode = key.split("/")
        verdicts = collections.Counter(r["verdict"] for r in groups[key])
        states = sorted({r.get("kind_state") for r in groups[key]
                         if r.get("kind_state")})
        print(f"| {phase} | {mode} | {len(groups[key])} | "
              f"{', '.join(f'{k} x{v}' for k, v in verdicts.items())} | "
              f"{', '.join(states)} |")
    bad_calib = [r for r in calib if r.get("kind") == "calibration"
                 and r.get("value") is False]
    print(f"\ncalibration failures: {len(bad_calib)}")
    for r in bad_calib:
        print("  CALIB-FAIL", r)


def sweep_table():
    recs = []
    for fn in glob.glob(os.path.join(ARGS.dir, "sweep*.jsonl")):
        recs.extend(load(fn))
    recs = [r for r in recs if "offset_us" in r]
    if not recs:
        return
    print("\n### Timing sweep (real SIGALRM, survivor mode)\n")
    print("| offset us | n | state classes | survivor effect |")
    print("|---|---|---|---|")
    by = collections.defaultdict(list)
    for r in recs:
        by[r["offset_us"]].append(r)
    for off in sorted(by):
        rs = by[off]
        classes = collections.Counter(r["class"] for r in rs)
        effects = collections.Counter(str(r.get("effect")) for r in rs)
        print(f"| {off:g} | {len(rs)} | "
              f"{', '.join(f'{k} x{v}' for k, v in classes.items())} | "
              f"{', '.join(f'{k} x{v}' for k, v in effects.items())} |")


def recovery_table(fn, title):
    recs = [r for r in load(fn)
            if r.get("kind") == "trial" and r.get("mode") == "recovery"]
    if not recs:
        return
    print(f"\n### {title}: recovery attempts after the interrupt\n")
    print("| phase | n | rollback() | raw ROLLBACK | reuse transaction() | plain insert | rows |")
    print("|---|---|---|---|---|---|---|")
    by = collections.defaultdict(list)
    for r in recs:
        by[r["key"].split("/")[0]].append(r)
    for phase, rs in sorted(by.items()):
        def step(rec, name):
            for s in rec.get("recovery") or []:
                if s.get("step") == name:
                    return s.get("result", "?")
            return "-"
        counts = collections.Counter()
        for r in rs:
            for name in ("rollback", "raw_rollback", "reuse_transaction",
                         "plain_insert"):
                val = step(r, name)
                counts[(name, "ok" if val == "ok" else "err")] += 1
        rows = sorted({tuple(r.get("rows") or []) for r in rs})
        print(f"| {phase} | {len(rs)} | "
              f"{counts[('rollback','ok')]} ok / {counts[('rollback','err')]} err | "
              f"{counts[('raw_rollback','ok')]} ok / {counts[('raw_rollback','err')]} err | "
              f"{counts[('reuse_transaction','ok')]} ok / {counts[('reuse_transaction','err')]} err | "
              f"{counts[('plain_insert','ok')]} ok / {counts[('plain_insert','err')]} err | "
              f"{rows} |")


def pool_table():
    rows = []
    for fn, label in (("pool.jsonl", "original"), ("poolfix.jsonl", "fix")):
        path = os.path.join(ARGS.dir, fn)
        recs = [r for r in load(path) if r.get("kind") == "pool"]
        for r in recs:
            rows.append((label, r))
    if not rows:
        return
    print("\n### Sync pool: release and the next checkout\n")
    print("| build | case | n | same backend reused | next transaction() | victim visible | verdicts |")
    print("|---|---|---|---|---|---|---|")
    by = collections.defaultdict(list)
    for label, r in rows:
        by[(label, r["case"])].append(r)
    for (label, case), rs in sorted(by.items()):
        reused = sum(1 for r in rs if r.get("same_backend"))
        results = collections.Counter(
            (r.get("victim") or {}).get("result") for r in rs)
        vis = collections.Counter(str(r.get("victim_visible")) for r in rs)
        verds = collections.Counter(r.get("verdict") for r in rs)
        print(f"| {label} | {case} | {len(rs)} | {reused}/{len(rs)} | "
              f"{', '.join(f'{k} x{v}' for k, v in results.items())} | "
              f"{', '.join(f'{k} x{v}' for k, v in vis.items())} | "
              f"{', '.join(f'{k} x{v}' for k, v in verds.items())} |")


def sigint_table():
    recs = [r for r in load(os.path.join(ARGS.dir, "sigint.jsonl"))
            if r.get("kind") == "sigint"]
    if not recs:
        return
    print("\n### Real SIGINT from a helper thread (entry wait)\n")
    print("| n | states | survivor visible |")
    print("|---|---|---|")
    states = collections.Counter(r.get("kind_state") for r in recs)
    vis = collections.Counter(str(r.get("survivor_visible")) for r in recs)
    print(f"| {len(recs)} | {dict(states)} | {dict(vis)} |")


def contrast_table():
    recs = load(os.path.join(ARGS.dir, "contrast.jsonl"))
    if not recs:
        return
    r = recs[-1]
    print("\n### Legitimate nesting vs abandoned transaction\n")
    for key in ("legit_inner_visible_inside", "legit_inner_visible_after_outer",
                "abandoned_client", "abandoned_retry",
                "abandoned_visible_after_retry", "abandoned_server",
                "dropped_client", "dropped_server", "dropped_inner_visible"):
        print(f"- {key}: {r.get(key)}")
    print("- wire SQL:", r.get("wire_sql"))


def fields_table():
    recs = [r for r in load(os.path.join(ARGS.dir, "fields.jsonl"))
            if r.get("kind") == "fields"]
    if not recs:
        return
    print("\n### Field-contract re-judgment of the same sync trials\n")
    print("| phase/mode | n | overall | per-field statuses |")
    print("|---|---|---|---|")
    by = collections.defaultdict(list)
    for r in recs:
        by[f"{r['phase']}/{r['mode']}"].append(r)
    for key, rs in sorted(by.items()):
        overall = collections.Counter(r["status"] for r in rs)
        per = collections.defaultdict(collections.Counter)
        for r in rs:
            for name, status in r["fields"].items():
                per[name][status] += 1
        per_txt = "; ".join(f"{name}: {dict(c)}" for name, c in per.items())
        print(f"| {key} | {len(rs)} | {dict(overall)} | {per_txt} |")


def main():
    global ARGS
    p = argparse.ArgumentParser()
    p.add_argument("--dir", default=".")
    ARGS = p.parse_args()
    phase_table(os.path.join(ARGS.dir, "phases2.jsonl"),
                "Original library: phase x mode")
    phase_table(os.path.join(ARGS.dir, "fix2.jsonl"),
                "Candidate fix installed: same injections")
    recovery_table(os.path.join(ARGS.dir, "phases2.jsonl"),
                   "Original library")
    sweep_table()
    pool_table()
    sigint_table()
    contrast_table()
    fields_table()


if __name__ == "__main__":
    main()
