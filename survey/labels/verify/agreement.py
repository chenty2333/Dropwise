#!/usr/bin/env python3
"""Agreement between annotators A and B, and between each human and the model.

Reads positives_{A,B}.csv / negatives_{A,B}.csv (joined by url) and ../../data/labels.jsonl.
For single-valued fields: raw agreement and Cohen's kappa over rows both parties filled in.
For multi-valued fields (violation, tags): each value is scored as a yes/no decision per row.
Also prints the disagreements to adjudicate.
"""
import csv
import json
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE.parent.parent / "data"
SINGLE = ["relevant", "holder", "phase", "after", "outlives", "cancel_source", "layer",
          "found_by", "injectable"]
MULTI = ["violation", "tags"]
V3 = ["holder", "phase", "after", "violation", "outlives", "cancel_source", "layer", "found_by",
      "violation_extra"]


def read(path):
    if not path.exists():
        return {}
    with path.open(newline="") as f:
        return {r["url"]: {k: (v or "").strip() for k, v in r.items()} for r in csv.DictReader(f)}


def kappa(pairs):
    """Cohen's kappa for a list of (a, b) labels."""
    n = len(pairs)
    if n == 0:
        return None, None
    po = sum(a == b for a, b in pairs) / n
    ca, cb = Counter(a for a, _ in pairs), Counter(b for _, b in pairs)
    pe = sum(ca[k] * cb[k] for k in ca) / (n * n)
    k = 1.0 if pe == 1 else (po - pe) / (1 - pe)
    return po, k


def split_v3(v):
    return frozenset(x.strip() for x in v.split(";") if x.strip())


def v3_round():
    a, b = read(HERE / "v3_A.csv"), read(HERE / "v3_B.csv")
    if not a or not b:
        return
    common = [u for u in a if u in b]
    print(f"\n######## codebook v3 (round 2): {len(common)} shared rows")
    for f in V3:
        conv = split_v3 if f == "violation_extra" else (lambda v: v)
        po, k = kappa([(conv(a[u].get(f, "")), conv(b[u].get(f, ""))) for u in common])
        print(f"  {f:16} agreement={po:5.1%}  kappa={k:5.2f}")
    t_a, t_b = read(HERE / "triage_A.csv"), read(HERE / "triage_B.csv")
    if t_a and t_b:
        pairs = [(t_a[u]["triage"], t_b[u]["triage"]) for u in t_a if u in t_b]
        po, k = kappa(pairs)
        c = Counter(pairs)
        print(f"\n######## triage: {len(pairs)} rows  agreement={po:5.1%}  kappa={k:5.2f}  {dict(c)}")


def split(v):
    return {x.strip() for x in v.split(";") if x.strip()}


def compare(name, x, y, fields, show):
    common = [u for u in x if u in y]
    print(f"\n== {name}: {len(common)} shared rows")
    if not common:
        return
    for f in fields:
        pairs = [(x[u].get(f, ""), y[u].get(f, "")) for u in common
                 if x[u].get(f) and y[u].get(f)]
        po, k = kappa(pairs)
        if po is not None:
            print(f"  {f:14} n={len(pairs):3}  agreement={po:5.1%}  kappa={k:5.2f}")
    for f in [m for m in MULTI if m in fields]:
        values = sorted({v for u in common for v in split(x[u].get(f, "")) | split(y[u].get(f, ""))})
        for v in values:
            pairs = [(v in split(x[u].get(f, "")), v in split(y[u].get(f, ""))) for u in common
                     if x[u].get(f) and y[u].get(f)]
            po, k = kappa(pairs)
            if po is not None:
                print(f"  {f}={v:18} n={len(pairs):3}  agreement={po:5.1%}  kappa={k:5.2f}")
    if show:
        print("  disagreements:")
        for u in common:
            diff = [f for f in fields if x[u].get(f) and y[u].get(f)
                    and (split(x[u][f]) if f in MULTI else x[u][f])
                    != (split(y[u][f]) if f in MULTI else y[u][f])]
            if diff:
                print(f"    {u}  " + ", ".join(f"{f}: {x[u][f]!r} vs {y[u][f]!r}" for f in diff))


def main():
    model = {r["url"]: {k: str(v) for k, v in r.items()} for r in map(json.loads, open(DATA / "labels.jsonl"))}
    for kind in ("positives", "negatives"):
        fields = SINGLE + MULTI
        a, b = read(HERE / f"{kind}_A.csv"), read(HERE / f"{kind}_B.csv")
        print(f"\n######## {kind}")
        compare("A vs B", a, b, fields, show=True)
        for who, h in (("A", a), ("B", b)):
            compare(f"{who} vs model", h, {u: model[u] for u in h if u in model}, fields, show=False)


if __name__ == "__main__":
    main()
    v3_round()
