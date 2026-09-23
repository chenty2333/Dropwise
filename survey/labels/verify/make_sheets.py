#!/usr/bin/env python3
"""Create blind annotation sheets for two human annotators.

  positives_{A,B}.csv  every row the model flagged (relevant yes/unclear/adjacent), shuffled,
                       no model labels
  negatives_{A,B}.csv  a stratified random sample of rows the model labelled relevant=no

Both annotators get the same rows (different order), so agreement can be computed.
Re-running with the same --seed reproduces the sheets; it refuses to overwrite sheets that
already contain annotations.
"""
import argparse
import csv
import json
import random
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE.parent.parent / "data"

POS_FIELDS = ["relevant", "holder", "phase", "after", "violation", "outlives", "tags",
              "cancel_source", "layer", "found_by", "injectable", "fix_url", "notes"]
# A sampled negative may turn out to be a defect, so negatives get the same fields.
NEG_FIELDS = POS_FIELDS
META = ["item", "url", "repo", "kind", "title"]


def load():
    cands = {r["url"]: r for r in map(json.loads, open(DATA / "candidates.jsonl"))}
    labels = {r["url"]: r for r in map(json.loads, open(DATA / "labels.jsonl"))}
    return cands, labels


def write(path, rows, fields):
    if path.exists():
        with path.open(newline="") as f:
            if any(any(r.get(k) for k in fields) for r in csv.DictReader(f)):
                raise SystemExit(f"{path} already has annotations; not overwriting")
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=META + fields)
        w.writeheader()
        for i, r in enumerate(rows, 1):
            w.writerow({"item": i, **{k: r.get(k, "") for k in META[1:]}, **{k: "" for k in fields}})


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--seed", type=int, default=20260923)
    ap.add_argument("--negatives", type=int, default=60, help="size of the negative sample")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    cands, labels = load()

    flagged = {"yes", "unclear", "adjacent"}
    pos = [cands[u] for u, l in labels.items() if l.get("relevant") in flagged and u in cands]

    # Negatives: stratify by (model confidence, repo) so low-confidence calls and every repo
    # are represented; allocate proportionally with at least one per stratum when possible.
    strata = defaultdict(list)
    for u, l in labels.items():
        if l.get("relevant") == "no" and u in cands:
            strata[(l.get("confidence", ""), cands[u]["repo"])].append(cands[u])
    total = sum(len(v) for v in strata.values())
    neg = []
    for key in sorted(strata):
        rows = strata[key]
        k = max(1, round(args.negatives * len(rows) / total))
        neg += rng.sample(rows, min(k, len(rows)))
    rng.shuffle(neg)
    neg = neg[: args.negatives]

    for who in "AB":
        p, n = pos[:], neg[:]
        rng.shuffle(p)
        rng.shuffle(n)
        write(HERE / f"positives_{who}.csv", p, POS_FIELDS)
        write(HERE / f"negatives_{who}.csv", n, NEG_FIELDS)
    print(f"{len(pos)} positives, {len(neg)} negatives (from {total} model negatives) -> {HERE}")


if __name__ == "__main__":
    main()
