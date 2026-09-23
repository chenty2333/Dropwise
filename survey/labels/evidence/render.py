import json, sys
rows = [json.loads(l) for l in open("labels/evidence/facts.jsonl")]
lo, hi = int(sys.argv[1]), int(sys.argv[2])
for i, f in enumerate(rows[lo:hi], lo + 1):
    u = f["url"].replace("https://github.com/", "")
    head = f"#{i} {u} [{f['kind']}"
    if f["kind"] == "pr":
        head += f" merged={f.get('merged')} by={f.get('merged_by')} tests={len(f.get('test_files',[]))}/{f.get('diff_adds_test_fn')}"
    else:
        head += f" {f.get('state')}/{f.get('state_reason')}"
    head += f" author={f.get('author_association')} code={f.get('body_has_code')}] {(f.get('title') or '')[:70]}"
    print(head)
    if f.get("closed_by_commit"): print("   closed_by_commit", f["closed_by_commit"][:10])
    for x in f.get("xref_prs", [])[:4]:
        print(f"   xref {'M' if x['merged'] else '-'} {x['url'].replace('https://github.com/','')} {x['title'][:60]}")
    for c in f.get("maintainer_comments", [])[:2]:
        print("   maint", c[:230])
    print("   body", (f.get("body") or f.get("message") or "")[:230])
