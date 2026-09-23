#!/usr/bin/env python3
"""Merge label batches into ../data/labels.jsonl.

Explicit labels come from batch*.py; dims*.py then add codebook-v2 dimensions and
corrections (their fields win; `notes_append` is appended to notes). Candidates listed in
reviewed*.txt (titles/bodies triaged) but not labelled explicitly get relevant=no.
"""
import importlib, json, re, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
DATA = HERE.parent / "data"
ANNOTATOR = "claude-opus-5-5"
DOCS = re.compile(r"\b(doc|docs|document|documentation|clarify|prepare|release|chore)\b", re.I)

cands = [json.loads(l) for l in open(DATA / "candidates.jsonl")]
def key(repo, number):
    return (repo, str(number))
by_key = {key(c["repo"], c["number"]): c for c in cands}

labels = {}
for batch in sorted(HERE.glob("batch*.py")):
    for repo, number, fields in importlib.import_module(batch.stem).LABELS:
        c = by_key.get(key(repo, number))
        if c is None:
            print(f"warning: {repo}#{number} not in candidates", file=sys.stderr)
            continue
        labels[c["url"]] = {"url": c["url"], **fields}

for dims in sorted(HERE.glob("dims*.py")):
    for repo, number, fields in importlib.import_module(dims.stem).DIMS:
        c = by_key.get(key(repo, number))
        if c is None or c["url"] not in labels:
            print(f"warning: dims for unlabelled {repo}#{number}", file=sys.stderr)
            continue
        lab = labels[c["url"]]
        fields = dict(fields)
        extra = fields.pop("notes_append", "")
        lab.update(fields)
        if extra:
            lab["notes"] = f"{lab.get('notes', '')}; {extra}".strip("; ")

reviewed = set()
for f in sorted(HERE.glob("reviewed*.txt")):
    reviewed |= set(f.read_text().split())
for c in cands:
    if c["url"] in reviewed and c["url"] not in labels:
        if DOCS.search(c["title"]):
            labels[c["url"]] = {"url": c["url"], "relevant": "no", "confidence": "high",
                                "notes": "docs/release/chore"}
        else:
            labels[c["url"]] = {"url": c["url"], "relevant": "no", "confidence": "medium",
                                "notes": "title/body triage: not a cancellation defect"}

with open(DATA / "labels.jsonl", "w") as f:
    for lab in labels.values():
        lab["annotator"] = ANNOTATOR
        f.write(json.dumps(lab, ensure_ascii=False) + "\n")
n = lambda v: sum(1 for l in labels.values() if l["relevant"] == v)
print(f"{len(labels)} labels: yes={n('yes')} adjacent={n('adjacent')} dup={n('dup')} "
      f"unclear={n('unclear')} no={n('no')}; "
      f"unreviewed={len(cands) - len(labels)}")
