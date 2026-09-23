#!/usr/bin/env python3
"""Collect raw evidence facts for every verified defect (labels/human/relevant.csv, yes).
Facts only; the evidence level is assigned by reading them (evidence.csv)."""
import csv, json, subprocess, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "facts.jsonl"
MAINT = {"MEMBER", "OWNER", "COLLABORATOR"}


def gh(path, paginate=False):
    cmd = ["gh", "api", path] + (["--paginate"] if paginate else [])
    for _ in range(3):
        p = subprocess.run(cmd, capture_output=True, text=True)
        if p.returncode == 0:
            out = p.stdout.strip()
            if paginate and out.startswith("[") and "][" in out:
                out = "[" + out[1:-1].replace("][", ",") + "]"
            return json.loads(out) if out else None
    print(f"failed: {path}: {p.stderr.strip()[:120]}", file=sys.stderr)
    return None


def short(s, n=400):
    return " ".join((s or "").split())[:n]


def facts(url):
    _, _, _, owner, repo, kind, num = url.split("/")[:7]
    base = f"repos/{owner}/{repo}"
    f = {"url": url}
    if kind == "commit":
        c = gh(f"{base}/commits/{num}")
        f.update(kind="commit", message=short(c and c["commit"]["message"], 600))
        return f
    issue = gh(f"{base}/issues/{num}") or {}
    f.update(kind="pr" if "pull_request" in issue else "issue", title=issue.get("title"),
             state=issue.get("state"), state_reason=issue.get("state_reason"),
             author_association=issue.get("author_association"),
             body=short(issue.get("body"), 900),
             body_has_code="```" in (issue.get("body") or ""))
    if f["kind"] == "pr":
        pr = gh(f"{base}/pulls/{num}") or {}
        f.update(merged=bool(pr.get("merged_at")), merged_at=pr.get("merged_at"),
                 merged_by=(pr.get("merged_by") or {}).get("login"))
        files = gh(f"{base}/pulls/{num}/files", paginate=True) or []
        f["test_files"] = [x["filename"] for x in files if "test" in x["filename"].lower()][:6]
        f["diff_adds_test_fn"] = any("#[test]" in (x.get("patch") or "") or "#[tokio::test]" in (x.get("patch") or "") for x in files)
    comments = gh(f"{base}/issues/{num}/comments", paginate=True) or []
    f["maintainer_comments"] = [f"[{c['user']['login']}] {short(c['body'], 300)}"
                                for c in comments if c.get("author_association") in MAINT][:4]
    f["n_comments"] = len(comments)
    tl = gh(f"{base}/issues/{num}/timeline", paginate=True) or []
    xrefs = []
    for e in tl:
        if e.get("event") == "cross-referenced":
            src = (e.get("source") or {}).get("issue") or {}
            if src.get("pull_request"):
                xrefs.append({"url": src.get("html_url"), "title": src.get("title"),
                              "merged": bool((src.get("pull_request") or {}).get("merged_at"))})
        if e.get("event") == "closed" and e.get("commit_id"):
            f["closed_by_commit"] = e["commit_id"]
    f["xref_prs"] = xrefs[:8]
    return f


def main():
    rel = [r for r in csv.DictReader(open(HERE.parent / "human" / "relevant.csv")) if r["relevant"] == "yes"]
    done = {json.loads(l)["url"] for l in OUT.open()} if OUT.exists() else set()
    with OUT.open("a") as out:
        for r in rel:
            if r["url"] in done:
                continue
            out.write(json.dumps(facts(r["url"]), ensure_ascii=False) + "\n")
            out.flush()
    print(f"{len(rel)} defects; facts in {OUT}")


if __name__ == "__main__":
    main()
