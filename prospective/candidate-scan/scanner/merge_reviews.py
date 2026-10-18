#!/usr/bin/env python3
"""Merge per-group review files into candidates.csv.

The review vocabulary is the three-way one from the task: 可疑 / 误报 / 不确定.
A reviewer's `已防护` is a kind of false positive for this scan (the hit is real
code but already guarded), so it is folded into 误报 and the note is prefixed.

Run after scan.py and after every reviewed-*.csv exists:
  python3 -I merge_reviews.py
"""
import collections
import csv
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
BASE = HERE.parent
REVIEWS = BASE / "reviews"

# Corrections applied after reading the source ourselves.  Keep these small and
# explain them: the CSV is a reviewed artifact, not a raw dump.
OVERRIDES = {
    # lettre: these try_smtp! call sites are reached during connection setup
    # (STARTTLS/EHLO/AUTH) or after QUIT; they are not the pooled send()
    # exchange.  H132 is a real quit-then-recycle bug, but it is not the
    # error/cancel asymmetry this scan targets.
    "H129": ("误报", "starttls 在连接建立阶段调用，连接尚未进入 lettre 池；不是可复用连接的交换。"),
    "H130": ("误报", "ehlo 在连接建立/STARTTLS 升级阶段调用，连接尚未进入池。"),
    "H131": ("误报", "ehlo 同上；多行回复残留只在建连阶段，连接随后才入池。"),
    "H132": ("误报", "quit 在 async-std 的 send_raw 末尾调用；cancel 只会丢掉 221 等待。quit 后仍入池是另一类缺陷（非取消特有），不在本目标内。"),
    "H133": ("误报", "AUTH 只在连接建立阶段调用，连接尚未进入池。"),
    "H096": ("可疑", "diesel-async 的 bb8 manager 只用默认 is_broken（TransactionManagerStatus，src/pooled_connection/mod.rs:209-211）；AsyncMysqlConnection 未覆写（src/mysql/mod.rs:366），不查 mysql_async 的 disconnected（src/conn/mod.rs:405-411）。取消在等首包时 pending_result 尚未设置（src/conn/routines/helpers.rs:44-73），clean_dirty 排不掉，下一次 Verified ping 会把旧回复当自己的结果。"),
    "H097": ("可疑", "默认 is_broken 只查 diesel 的 TransactionManagerStatus（src/pooled_connection/mod.rs:209-211）；AsyncMysqlConnection 的空 impl（src/mysql/mod.rs:366）没有覆写它去查 mysql_async 的 disconnected。取消落在 mysql_async read_packet 等首包时，pending_result 仍为 None（src/conn/routines/helpers.rs:44-73），旧回复留在 socket；bb8/deadpool 的 Verified ping 可能把它当 SELECT 1 的结果，随后连接以错位状态回池。"),
}

# A reviewer may have used 已防护.  Candidates.csv uses the task's three
# categories; fold 已防护 into 误报 but keep the reason.
FOLD = {"已防护": "误报"}


def main():
    work = list(csv.DictReader((REVIEWS / "worklist-all.csv").open()))
    by_id = {r["id"]: dict(r) for r in work}
    order = [r["id"] for r in work]
    for p in sorted(REVIEWS.glob("reviewed-*.csv")):
        for r in csv.DictReader(p.open()):
            if r["id"] not in by_id:
                raise SystemExit(f"unknown id {r['id']} in {p.name}")
            by_id[r["id"]]["review"] = r["review"].strip()
            by_id[r["id"]]["note"] = r["note"].strip()
    for i, row in by_id.items():
        row.setdefault("guarded", "no")
        if not row.get("review"):
            row["review"] = "未复核"
            row["note"] = ""
        if row["review"] in FOLD:
            row["note"] = "已防护：" + row["note"]
            row["review"] = FOLD[row["review"]]
        if i in OVERRIDES:
            row["review"], row["note"] = OVERRIDES[i]

    fields = ["id", "rank", "downloads_90d", "crate", "version", "file", "line",
              "pattern", "confidence", "guarded", "detail", "snippet", "review", "note"]
    out = BASE / "candidates.csv"
    with out.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for i in order:
            r = by_id[i]
            w.writerow({k: r.get(k, "") for k in fields})
    counts = collections.Counter(by_id[i]["review"] for i in order)
    print(f"wrote {out} rows={len(order)} {dict(counts)}")
    pat = collections.Counter((by_id[i]["pattern"], by_id[i]["review"]) for i in order)
    for k in sorted(pat):
        print(k, pat[k])


if __name__ == "__main__":
    main()
