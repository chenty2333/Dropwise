#!/usr/bin/env python3
"""Blind triage sheets for candidates that no annotator has seen yet (symptom round).

  triage_{A,B}.csv: url, repo, kind, state, title, excerpt, triage, notes
  triage = keep (could be an in-scope cancellation defect: read in full later) | drop

Both annotators triage every row independently; the union of `keep` goes to full annotation.
Rows already labelled (model labels or verified sheets) are excluded.
"""
import csv
import json
import random
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE.parent.parent / "data"
FIELDS = ["item", "url", "repo", "kind", "state", "title", "excerpt", "triage", "notes"]


def main(seed=20260925):
    labelled = {r["url"] for r in map(json.loads, open(DATA / "labels.jsonl"))}
    for f in HERE.glob("*_A.csv"):
        labelled |= {r["url"] for r in csv.DictReader(f.open())}
    rows = [r for r in map(json.loads, open(DATA / "candidates.jsonl")) if r["url"] not in labelled]
    for who in "AB":
        path = HERE / f"triage_{who}.csv"
        if path.exists() and any(r.get("triage") for r in csv.DictReader(path.open())):
            print(f"{path.name} already has answers; not overwritten")
            continue
        rng = random.Random(f"{seed}{who}")
        order = rows[:]
        rng.shuffle(order)
        with path.open("w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=FIELDS)
            w.writeheader()
            for i, r in enumerate(order, 1):
                w.writerow({"item": i, "url": r["url"], "repo": r["repo"], "kind": r["kind"],
                            "state": r["state"], "title": r["title"],
                            "excerpt": " ".join(r.get("body", "").split())[:500],
                            "triage": "", "notes": ""})
    print(f"{len(rows)} untriaged candidates -> triage_A.csv / triage_B.csv")


if __name__ == "__main__":
    main()
