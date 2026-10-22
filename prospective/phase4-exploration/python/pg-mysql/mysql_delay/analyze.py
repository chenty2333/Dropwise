"""Summarise the round-5 mysql_delay JSONL records.

Usage: python mysql_delay/analyze.py RUNDIR/records.jsonl

Writes a compact table to stdout and `analysis.json` next to the records.
All durations are milliseconds from the *first* cancel request unless the
field name says otherwise (a2 = from the second cancel request).  `None`
means the event was not observed / unrealised, not zero.
"""

from __future__ import annotations

import collections
import json
import os
import statistics
import sys


def get(rec, path, default=None):
    cur = rec
    for part in path.split("."):
        if not isinstance(cur, dict):
            return default
        cur = cur.get(part)
        if cur is None:
            return default
    return cur


def collect(rec, *paths):
    out = []
    for path in paths:
        value = get(rec, path)
        if isinstance(value, (int, float)):
            out.append(float(value))
    return out


def med(values):
    return round(statistics.median(values), 2) if values else None


def rng(values):
    if not values:
        return None
    return [round(min(values), 2), round(max(values), 2)]


def counter(values):
    return dict(collections.Counter(values))


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    path = sys.argv[1]
    records = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    if not records:
        print("no records")
        return 1

    groups: dict[tuple, list[dict]] = collections.defaultdict(list)
    for rec in records:
        meta = rec["meta"]
        escalate = "--escalate" in (meta.get("argv") or [])
        mode = meta["mode"] + ("+esc" if escalate else "")
        key = (meta["build"], meta["target"], meta["delay_s"],
               meta["strategy"], mode)
        groups[key].append(rec)

    summary = []
    for key in sorted(groups):
        build, target, delay, strategy, mode = key
        recs = groups[key]
        a = [v for r in recs for v in collect(r, "target.a_ms")]
        b = [v for r in recs for v in collect(r, "resource.b_ms")]
        tx_closed = [v for r in recs for v in collect(r, "resource.tx_closed_ms")]
        evicted = [v for r in recs for v in collect(r, "resource.evicted_ms")]
        proxy_evicted = [v for r in recs
                         for v in collect(r, "resource.proxy_evicted_ms")]
        cleanup = [v for r in recs
                   for v in collect(r, "resource.cleanup_close_ms")]
        recovery_ms = [v for r in recs
                       for v in collect(r, "recovery.recovery_ms")]
        a2 = [v for r in recs
              for v in collect(r, "second_cancel.a2_ms")]
        primary_ms = [v for r in recs
                      for v in collect(r, "successor.usable_ms")]
        esc_ms = [v for r in recs
                  for v in collect(r, "escalation.successor.usable_ms")]

        target_conn_close = []
        for r in recs:
            t_cancel = get(r, "timeline.t_cancel_request")
            conn = get(r, "target.stale_reply.chunk.conn")
            close_t = (get(r, "proxy.conn_close_t", {}) or {}).get(str(conn))
            close_ms = None
            if t_cancel is not None and close_t is not None:
                close_ms = round((close_t - t_cancel) * 1000.0, 2)
            elif (t_cancel is not None and get(r, "escalation") is None
                  and get(r, "resource.cleanup_close_ms") is not None):
                # No escalation: cleanup closed the still-held target handle.
                close_ms = get(r, "resource.cleanup_close_ms")
            if close_ms is not None:
                target_conn_close.append(close_ms)

        stale_delivery = []
        for r in recs:
            t_cancel = get(r, "timeline.t_cancel_request")
            seq = get(r, "target.stale_reply.chunk_seq")
            chunks = get(r, "proxy.chunks", []) or []
            for chunk in chunks:
                if chunk.get("seq") == seq and t_cancel and chunk.get("deliver_t"):
                    stale_delivery.append(
                        round((chunk["deliver_t"] - t_cancel) * 1000.0, 2))
                    break

        entry = {
            "build": build, "target": target, "delay_s": delay,
            "strategy": strategy, "mode": mode, "n": len(recs),
            "target_outcome": counter(get(r, "target.outcome") for r in recs),
            "a_ms": {"median": med(a), "range": rng(a), "n": len(a)},
            "b_ms": {"median": med(b), "range": rng(b), "n": len(b),
                     "kinds": counter(get(r, "resource.b_kind") for r in recs),
                     "unknown": sum(1 for r in recs
                                    if get(r, "resource.b_ms") is None)},
            "tx_closed_ms": {"median": med(tx_closed), "range": rng(tx_closed),
                             "n": len(tx_closed)},
            "evicted_ms": {"median": med(evicted), "range": rng(evicted),
                           "n": len(evicted)},
            "proxy_evicted_ms": {"median": med(proxy_evicted),
                                 "range": rng(proxy_evicted),
                                 "n": len(proxy_evicted)},
            "cleanup_close_ms": {"median": med(cleanup),
                                 "range": rng(cleanup), "n": len(cleanup)},
            "target_conn_close_ms": {"median": med(target_conn_close),
                                     "range": rng(target_conn_close),
                                     "n": len(target_conn_close)},
            "recovery_outcome": counter(
                get(r, "recovery.outcome") or get(r, "recovery.task_outcome")
                for r in recs),
            "recovery_on_caller_cancelled": sum(
                1 for r in recs if get(r, "recovery.task_outcome") == "cancelled"),
            "recovery_ms": {"median": med(recovery_ms),
                            "range": rng(recovery_ms), "n": len(recovery_ms)},
            "stale_reply_held": sum(
                1 for r in recs if get(r, "target.stale_reply.held_at_cancel")),
            "stale_reply_delivered_before_cancel": sum(
                1 for r in recs
                if get(r, "target.stale_reply.delivered_before_cancel")),
            "stale_delivery_ms": {"median": med(stale_delivery),
                                  "range": rng(stale_delivery),
                                  "n": len(stale_delivery)},
            "primary_usable": sum(1 for r in recs
                                  if get(r, "successor.usable")),
            "primary_client_ok": sum(1 for r in recs
                                     if get(r, "successor.client_ok")),
            "primary_observer_visible": sum(
                1 for r in recs if get(r, "successor.observer_visible")),
            "primary_write_rowcounts": counter(
                get(r, "successor.client_write_rowcount") for r in recs),
            "primary_readbacks": counter(
                json.dumps(get(r, "successor.client_readback")) for r in recs),
            "primary_usable_ms": {"median": med(primary_ms),
                                  "range": rng(primary_ms),
                                  "n": len(primary_ms)},
            "escalation_successor_present": sum(
                1 for r in recs if get(r, "escalation.successor") is not None),
            "escalation_usable": sum(
                1 for r in recs if get(r, "escalation.successor.usable")),
            "escalation_usable_ms": {"median": med(esc_ms),
                                     "range": rng(esc_ms), "n": len(esc_ms)},
            "second_realized": sum(
                1 for r in recs if get(r, "second_cancel.realized") is True),
            "second_unrealized_reasons": counter(
                get(r, "second_cancel.reason") for r in recs
                if get(r, "second_cancel.realized") is False),
            "second_suspension_kinds": counter(
                get(r, "second_cancel.suspension_kind") for r in recs
                if get(r, "second_cancel.realized") is True),
            "second_saw_recovery_action": sum(
                1 for r in recs
                if get(r, "second_cancel.recovery_saw_cancel") is True),
            "second_caller_cancelled": sum(
                1 for r in recs
                if get(r, "second_cancel.recovery_outcome.outcome")
                == "cancelled"),
            "a2_ms": {"median": med(a2), "range": rng(a2), "n": len(a2)},
            "outer_visible": sum(
                1 for r in recs if get(r, "resource.outer_visible_ms") is not None),
            "outer_visible_ms": {
                "median": med([v for r in recs
                               for v in collect(r, "resource.outer_visible_ms")]),
                "n": sum(1 for r in recs
                         if get(r, "resource.outer_visible_ms") is not None),
            },
            "successor_visible": sum(
                1 for r in recs
                if get(r, "resource.successor_visible_ms") is not None),
            "errors": counter(
                e for r in recs for e in (r.get("errors") or [])),
            "seconds": {"median": med([r["seconds"] for r in recs
                                       if "seconds" in r]),
                        "range": rng([r["seconds"] for r in recs
                                      if "seconds" in r])},
        }
        summary.append(entry)

    print(f"records={len(records)} cells={len(summary)}")
    hdr = (f"{'build':8} {'target':9} {'delay':>5} {'strategy':24} "
           f"{'mode':8} {'n':>2} {'a_med':>6} {'b_med':>7} {'b_kind':>10} "
           f"{'rec_med':>8} {'usable':>6} {'usable_ms':>10} {'2nd':>4} "
           f"{'saw':>3} {'a2_med':>7} {'outer':>5} {'cln_ms':>9} "
           f"{'conn_ms':>9}")
    print(hdr)
    print("-" * len(hdr))
    for e in summary:
        bkind = ",".join(f"{k}:{v}" for k, v in
                         sorted(e["b_ms"]["kinds"].items()))
        print(f"{e['build']:8} {e['target']:9} {e['delay_s']:>5} "
              f"{e['strategy']:24} {e['mode']:8} {e['n']:>2} "
              f"{str(e['a_ms']['median']):>6} {str(e['b_ms']['median']):>7} "
              f"{bkind[:10]:>10} "
              f"{str(e['recovery_ms']['median']):>8} "
              f"{e['primary_usable']:>3}/{e['n']:<2} "
              f"{str(e['primary_usable_ms']['median']):>10} "
              f"{e['second_realized']:>2}/{e['n']:<1} "
              f"{e['second_saw_recovery_action']:>3} "
              f"{str(e['a2_ms']['median']):>7} "
              f"{e['outer_visible']:>2}/{e['n']:<2} "
              f"{str(e['cleanup_close_ms']['median']):>9} "
              f"{str(e['target_conn_close_ms']['median']):>9}")

    out_dir = os.path.dirname(os.path.abspath(path))
    with open(os.path.join(out_dir, "analysis.json"), "w",
              encoding="utf-8") as fh:
        json.dump(summary, fh, indent=1, sort_keys=True)
        fh.write("\n")
    print()
    print("detail per cell:")
    for e in summary:
        print(json.dumps(e, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
