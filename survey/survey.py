#!/usr/bin/env python3
"""Collect and summarize candidate cancellation bugs in async Rust projects.

Uses the GitHub search API through the `gh` CLI (must be logged in).

  survey.py collect [--repos FILE | --repo R ...] [--global] [--max-pages N]
  survey.py export     # candidates.jsonl -> annotate.csv (keeps existing annotations)
  survey.py stats      # summarize annotate.csv
"""

import argparse
import csv
import json
import re
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE / "data"
CANDIDATES = DATA / "candidates.jsonl"
ANNOTATE = DATA / "annotate.csv"

# GitHub search allows at most 5 boolean operators per query, so each group is at most
# 5 phrases joined with OR.
TERMSETS = {
    # Vocabulary of people who already think in terms of cancellation.
    "cancel": [
        ["cancel safe", "cancel-safe", "cancel safety", "cancellation safety", "cancel-unsafe"],
        ["not cancel safe", "cancellation bug", "future is dropped", "future was dropped", "futurelock"],
        ["dropped future", "cancelled future", "select cancel", "timeout cancel", "cancel correctness"],
    ],
    # Symptoms, for bugs whose reporters never say "cancel safety".
    "symptom": [
        ["lost message", "lost messages", "message lost", "messages lost", "dropped messages"],
        ["select loop", "select! loop", "select! branch", "select! arm", "in a select!"],
        ["cancelled mid", "canceled mid", "cancelled in the middle", "canceled in the middle", "cancelled halfway"],
        ["request is cancelled", "request was cancelled", "handler is cancelled", "handler was cancelled",
         "client disconnects"],
        ["future gets dropped", "future being dropped", "gets cancelled", "got cancelled", "drop the future"],
        ["partially written", "partial write", "left in an inconsistent state", "half-written", "half written"],
    ],
}
TERMS = [t for groups in TERMSETS.values() for g in groups for t in g]

# Local hints for triage; they do not decide the category.
HINTS = {
    "select": r"select!|tokio::select|\bselect\b",
    "timeout": r"\btimeout\b",
    "abort": r"\babort(ed)?\b|JoinSet|AbortHandle",
    "join_handle": r"JoinHandle",
    "lost_data": r"\b(lost|lose|losing|dropp?ed) (a |the )?(message|data|item|bytes|frame|request)",
    "partial": r"partial(ly)?|half[- ]|mid[- ](frame|message|stream)|torn",
    "wrapper": r"wrapper|helper|internally|inside (the|this) (future|fn|function)",
    "read_write": r"read_exact|write_all|read_line|read_to_end|read_buf",
    "lock": r"\bMutex\b|\block\(\)",
    "stream": r"\bStream\b|\.next\(\)|recv\(\)",
    "deadlock": r"deadlock|hang|futurelock",
}

# Dependency bumps quote upstream changelogs ("cancel safety") and are pure noise.
NOISE_TITLE = re.compile(
    r"^(bump|update rust crate|update dependency|chore\(deps\)|build\(deps\)|\[?deps?\]?:|lockfile)",
    re.IGNORECASE)
NOISE_QUALIFIERS = "-author:app/dependabot -author:app/renovate"

# The documented limit is 30 searches/minute, but bursts trip the secondary rate limit.
SEARCH_INTERVAL = 4.0


def gh_search(kind, query, page):
    """One page of results, or None on error (logged)."""
    cmd = ["gh", "api", "-X", "GET", f"search/{kind}", "-f", f"q={query}",
           "-f", "per_page=100", "-f", f"page={page}"]
    for attempt in range(6):
        proc = subprocess.run(cmd, capture_output=True, text=True)
        if proc.returncode == 0:
            return json.loads(proc.stdout)
        err = proc.stderr.strip()
        if "rate limit" in err.lower() or "HTTP 403" in err or "HTTP 429" in err:
            wait = 30 * (attempt + 1)
            print(f"  rate limited, sleeping {wait}s", file=sys.stderr)
            time.sleep(wait)
            continue
        print(f"  error for {kind} `{query}` page {page}: {err[:200]}", file=sys.stderr)
        return None
    print(f"  error for {kind} `{query}` page {page}: gave up after rate limiting", file=sys.stderr)
    return None


def term_groups(termset):
    for group in TERMSETS[termset]:
        yield " OR ".join(f'"{t}"' for t in group)


def text_of(rec):
    return f"{rec.get('title', '')}\n{rec.get('body', '')}"


def annotate_matches(rec):
    text = text_of(rec).lower()
    rec["matched_terms"] = [t for t in TERMS if t.lower() in text]
    raw = text_of(rec)
    rec["hints"] = [h for h, pat in HINTS.items() if re.search(pat, raw, re.IGNORECASE)]
    # Rough triage order: strong terms + data/state symptoms first.
    rec["score"] = 2 * len(rec["matched_terms"]) + len(rec["hints"])
    return rec


def from_issue(item):
    return annotate_matches({
        "url": item["html_url"],
        "repo": "/".join(item["repository_url"].split("/")[-2:]),
        "kind": "pr" if "pull_request" in item else "issue",
        "number": item["number"],
        "title": item["title"],
        "state": item["state"],
        "merged": bool((item.get("pull_request") or {}).get("merged_at")),
        "created": item["created_at"][:10],
        "body": (item.get("body") or "")[:4000],
    })


def from_commit(item):
    msg = item["commit"]["message"]
    title, _, body = msg.partition("\n")
    return annotate_matches({
        "url": item["html_url"],
        "repo": item["repository"]["full_name"],
        "kind": "commit",
        "number": item["sha"][:12],
        "title": title,
        "state": "committed",
        "merged": True,
        "created": item["commit"]["committer"]["date"][:10],
        "body": body.strip()[:4000],
    })


def load_candidates():
    if not CANDIDATES.exists():
        return {}
    with CANDIDATES.open() as f:
        return {r["url"]: r for r in map(json.loads, f)}


def save_candidates(cands):
    DATA.mkdir(exist_ok=True)
    rows = sorted(cands.values(), key=lambda r: (-r["score"], r["repo"], str(r["number"])))
    with CANDIDATES.open("w") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def cmd_collect(args):
    scopes = []
    if args.global_:
        scopes.append("language:rust")
    repos = list(args.repo or [])
    if args.repos:
        repos += [l.strip() for l in Path(args.repos).read_text().splitlines()
                  if l.strip() and not l.startswith("#")]
    scopes += [f"repo:{r}" for r in repos]
    if not scopes:
        sys.exit("nothing to search: pass --repos, --repo or --global")

    kinds = [("issues", from_issue), ("commits", from_commit)]
    if args.no_commits:
        kinds = kinds[:1]

    cands = load_candidates()
    before = len(cands)
    for scope in scopes:
        for group in term_groups(args.termset):
            for kind, convert in kinds:
                query = f"{group} {scope}"
                if kind == "issues":
                    query += f" {NOISE_QUALIFIERS}"
                for page in range(1, args.max_pages + 1):
                    time.sleep(SEARCH_INTERVAL)
                    res = gh_search(kind, query, page)
                    if not res:
                        break
                    items = res.get("items", [])
                    for item in items:
                        rec = convert(item)
                        if NOISE_TITLE.search(rec["title"]):
                            continue
                        is_new = rec["url"] not in cands
                        rec = cands.setdefault(rec["url"], rec)
                        # Records from before termsets existed came from the "cancel" round.
                        found = rec.setdefault("termsets", [] if is_new else ["cancel"])
                        if args.termset not in found:
                            found.append(args.termset)
                    print(f"{scope:45} {kind:8} page {page}: {len(items):3} hits "
                          f"(total {res.get('total_count', '?')})", file=sys.stderr)
                    if len(items) < 100:
                        break
        save_candidates(cands)  # checkpoint per scope
    print(f"{len(cands) - before} new candidates, {len(cands)} total -> {CANDIDATES}")


ANNOTATION_FIELDS = ["relevant", "holder", "phase", "after", "violation", "violation_extra",
                     "outlives", "cancel_source", "layer", "found_by", "fix_url",
                     "reproduced", "evidence", "evidence_flag", "source_cluster", "notes", "annotator", "confidence",
                     # removed in codebook v3 / v1 leftovers; kept so old labels still load
                     "tags", "injectable", "category", "consequence"]
MULTI = {"violation_extra", "tags"}
EXPORT_FIELDS = ["score", "repo", "kind", "number", "state", "merged", "created", "title",
                 "termsets", "matched_terms", "hints", "url"] + ANNOTATION_FIELDS
# Model-produced labels (labels/apply.py writes them).
LABELS = DATA / "labels.jsonl"
# Human labels: any CSV here with a `url` column plus annotation columns. Non-empty
# cells override model labels. annotate.csv itself is a generated view; don't edit it.
HUMAN = HERE / "labels" / "human"


def load_labels():
    if not LABELS.exists():
        return {}
    with LABELS.open() as f:
        return {r["url"]: r for r in map(json.loads, f) if r.get("url")}


def load_human():
    out = {}
    for path in sorted(HUMAN.glob("*.csv")) if HUMAN.exists() else []:
        with path.open(newline="") as f:
            for r in csv.DictReader(f):
                if r.get("url"):
                    out.setdefault(r["url"], {}).update({k: v for k, v in r.items() if v and k != "url"})
    return out


def load_annotations():
    """Candidates joined with model labels, then human labels on top."""
    labels, human = load_labels(), load_human()
    rows = {}
    for rec in load_candidates().values():
        row = {k: rec.get(k, "") for k in EXPORT_FIELDS if k not in ANNOTATION_FIELDS}
        row["matched_terms"] = "; ".join(rec.get("matched_terms", []))
        row["hints"] = "; ".join(rec.get("hints", []))
        # Rows collected before termsets existed came from the "cancel" round.
        row["termsets"] = "; ".join(rec.get("termsets") or ["cancel"])
        for k in ANNOTATION_FIELDS:
            row[k] = labels.get(rec["url"], {}).get(k, "")
        for k, v in human.get(rec["url"], {}).items():
            if k in ANNOTATION_FIELDS:
                row[k] = v
        rows[rec["url"]] = row
    return rows


def cmd_export(_args):
    rows = sorted(load_annotations().values(), key=lambda r: (-int(r["score"] or 0), r["repo"]))
    DATA.mkdir(exist_ok=True)
    with ANNOTATE.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=EXPORT_FIELDS)
        w.writeheader()
        w.writerows(rows)
    labelled = sum(1 for r in rows if r["relevant"])
    print(f"{len(rows)} rows ({labelled} labelled) -> {ANNOTATE}")


def pct(n, d):
    return f"{n:4d}  ({100 * n / d:5.1f}%)" if d else f"{n:4d}"


def values(row, field):
    raw = row.get(field) or ""
    if field in MULTI:
        return [v.strip() for v in raw.split(";") if v.strip()] or ["(blank)"]
    return [raw or "(blank)"]


def breakdown(rows, field, indent="  "):
    counts = Counter(v for r in rows for v in values(r, field))
    for value, n in counts.most_common():
        print(f"{indent}{value:22} {pct(n, len(rows))}")


def crosstab(rows, a, b):
    cols = sorted({v for r in rows for v in values(r, b)})
    print(f"  {a + ' \\ ' + b:22} " + " ".join(f"{c[:12]:>12}" for c in cols))
    for va in sorted({v for r in rows for v in values(r, a)}):
        cells = [sum(1 for r in rows if va in values(r, a) and c in values(r, b)) for c in cols]
        print(f"  {va:22} " + " ".join(f"{n or '.':>12}" for n in cells))


def cmd_stats(args):
    rows = list(load_annotations().values())
    if not rows:
        sys.exit(f"no annotations: run `export` and fill in {ANNOTATE}")
    status = Counter((r["relevant"] or "(untriaged)").strip().lower() for r in rows)
    print(f"candidates {len(rows)}: " + ", ".join(f"{k}={v}" for k, v in status.most_common()))
    rel = [r for r in rows if r["relevant"].strip().lower() == "yes"]
    if args.verified:
        path = HUMAN / "relevant.csv"
        verified = {r["url"] for r in csv.DictReader(path.open())} if path.exists() else set()
        rel = [r for r in rel if r["url"] in verified]
        print(f"restricted to rows verified by two annotators ({len(verified)} rows checked)")
    if args.layer:
        rel = [r for r in rel if r["layer"] == args.layer]
        print(f"restricted to layer={args.layer}")
    print(f"counted (relevant=yes): {len(rel)}\n")
    if not rel:
        return
    for field in ["layer", "holder", "phase", "after", "violation", "violation_extra", "outlives",
                  "cancel_source", "found_by", "reproduced", "evidence", "evidence_flag", "termsets"]:
        print(f"{field}:")
        breakdown(rel, field)
        print()
    print("repos:")
    for value, n in Counter(r["repo"] for r in rel).most_common():
        print(f"  {value:40} {n}")

    app = [r for r in rel if r["layer"] == "app"]
    if app:
        print(f"\napp layer ({len(app)}): holder x after")
        crosstab(app, "holder", "after")
        print(f"\napp layer ({len(app)}): holder x phase")
        crosstab(app, "holder", "phase")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("collect", help="search GitHub and append to candidates.jsonl")
    c.add_argument("--repos", help="file with owner/repo per line")
    c.add_argument("--repo", action="append", help="owner/repo (repeatable)")
    c.add_argument("--global", dest="global_", action="store_true", help="also search all Rust repos")
    c.add_argument("--max-pages", type=int, default=3, help="pages of 100 per query (search caps at 10)")
    c.add_argument("--no-commits", action="store_true", help="skip commit search")
    c.add_argument("--termset", choices=sorted(TERMSETS), default="cancel",
                   help="which phrase set to search for")
    c.set_defaults(func=cmd_collect)
    sub.add_parser("export", help="write annotate.csv").set_defaults(func=cmd_export)
    st = sub.add_parser("stats", help="summarize annotate.csv")
    st.add_argument("--layer", choices=["app", "library"], help="only count this layer")
    st.add_argument("--verified", action="store_true",
                    help="only rows verified by the two independent annotators")
    st.set_defaults(func=cmd_stats)
    args = p.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
