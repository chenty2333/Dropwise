#!/usr/bin/env python3
"""Build verified labels from the two independent annotators.

Round 1 (codebook v2.1 sheets *_A.csv / *_B.csv) decides `relevant`:
  A == B -> that value; otherwise the adjudicated value from adjudicated_relevant.csv.
Writes ../human/relevant.csv and blind round-2 sheets v3_{A,B}.csv (codebook v3 dimensions)
for every row whose final relevant is `yes`.

Round 2 (v3_{A,B}.csv, once filled) decides the dimensions: A == B -> that value; otherwise
the value from adjudicated_v3.csv if present, else left as `disputed`. Writes ../human/v3.csv.
"""
import csv
import random
from pathlib import Path

HERE = Path(__file__).resolve().parent
HUMAN = HERE.parent / "human"
V3_FIELDS = ["holder", "phase", "after", "violation", "violation_extra", "outlives",
             "cancel_source", "layer", "found_by"]
META = ["item", "url", "repo", "kind", "title"]


def read(path):
    if not path.exists():
        return {}
    with path.open(newline="") as f:
        return {r["url"]: r for r in csv.DictReader(f)}


def write(path, rows, fields):
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        w.writerows(rows)


def round1():
    adj = read(HERE / "adjudicated_relevant.csv")
    out, meta = [], {}
    for kind in ("positives", "negatives", "full"):
        a, b = read(HERE / f"{kind}_A.csv"), read(HERE / f"{kind}_B.csv")
        if not a:
            continue
        for u in a:
            meta[u] = a[u]
            if a[u]["relevant"] == b[u]["relevant"]:
                rel, fix, basis = a[u]["relevant"], a[u]["fix_url"] or b[u]["fix_url"], "agree"
            elif u in adj:
                rel, fix, basis = adj[u]["relevant"], adj[u]["fix_url"], "adjudicated"
            else:
                raise SystemExit(f"unadjudicated disagreement: {u}")
            out.append({"url": u, "relevant": rel, "fix_url": fix, "basis": basis, "sheet": kind})
    # Cross-round duplicates (a row verified in one round duplicating one from another).
    for u, d in read(HERE / "dedup.csv").items():
        for r in out:
            if r["url"] == u:
                r.update(relevant=d["relevant"], fix_url=d["fix_url"], basis=r["basis"] + "+dedup")
    HUMAN.mkdir(exist_ok=True)
    write(HUMAN / "relevant.csv", out, ["url", "relevant", "fix_url", "basis", "sheet"])
    return out, meta


def make_v3_sheets(final, meta, seed=20260924):
    yes = [meta[r["url"]] for r in final if r["relevant"] == "yes"]
    rng = random.Random(seed)
    for who in "AB":
        path = HERE / f"v3_{who}.csv"
        if path.exists() and any(r.get("holder") for r in read(path).values()):
            print(f"{path.name} already filled; not regenerated")
            continue
        rows = yes[:]
        rng.shuffle(rows)
        write(path, [{"item": i, **{k: r[k] for k in META[1:]}, **{k: "" for k in V3_FIELDS},
                      "notes": ""} for i, r in enumerate(rows, 1)], META + V3_FIELDS + ["notes"])
    return len(yes)


def round2(final):
    """Dimensions for every final-yes row, from v3_{A,B} (round 2) or full_{A,B} (round 3).
    Both filled and equal -> value; both filled and different -> adjudicated_v3.csv or
    `disputed`; only one annotator filled (the other said not-yes) -> that value, basis single."""
    yes = {r["url"] for r in final if r["relevant"] == "yes"}
    adj = read(HERE / "adjudicated_v3.csv")
    sources = [(read(HERE / "v3_A.csv"), read(HERE / "v3_B.csv")),
               (read(HERE / "full_A.csv"), read(HERE / "full_B.csv"))]
    out, disputed, single = [], 0, 0
    for u in sorted(yes):
        a, b = next(((sa.get(u), sb.get(u)) for sa, sb in sources if u in sa), (None, None))
        if a is None:
            continue
        a_ok, b_ok = bool(a.get("holder")), bool(b and b.get("holder"))
        row = {"url": u, "basis": "double" if a_ok and b_ok else "single"}
        single += row["basis"] == "single"
        for f in V3_FIELDS:
            va, vb = a.get(f, ""), (b or {}).get(f, "")
            if not (a_ok and b_ok):
                row[f] = va if a_ok else vb
                continue
            same = (set(va.split(";")) == set(vb.split(";"))) if f == "violation_extra" else va == vb
            if f == "violation_extra" and not same:
                row[f] = ""  # kappa 0.30: not adjudicated, kept only where both agree
                continue
            row[f] = va if same else (adj.get(u, {}).get(f) or "disputed")
            disputed += row[f] == "disputed"
        out.append(row)
    write(HUMAN / "v3.csv", out, ["url", "basis"] + V3_FIELDS)
    return len(out), disputed, single


if __name__ == "__main__":
    final, meta = round1()
    n = make_v3_sheets(final, meta)
    counts = {}
    for r in final:
        counts[r["relevant"]] = counts.get(r["relevant"], 0) + 1
    print(f"round 1: {len(final)} rows -> {counts}; {n} yes rows in v3 sheets")
    n, disputed, single = round2(final)
    print(f"dimensions: {n} yes rows ({single} single-annotated), {disputed} disputed cells remain")
