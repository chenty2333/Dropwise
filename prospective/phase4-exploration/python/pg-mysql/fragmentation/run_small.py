"""Smallest finite group-3 run: fragmentation modes, entry cuts, send backpressure.

Run from prospective/phase4-exploration/python/pg-mysql with
PYTHONPATH=".:$REPO/research/python-retro".  New files only; the existing
cases, proxy and engine are imported read-only.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import random
import string
import sys
import time
import traceback
from pathlib import Path

import psycopg

import cases.txphase_case as tx
from cases.common import DELAY, bound, close_quiet, unique_table
from dropwise_py.engine import _await_target
from fragmentation.fragment_proxy import FragmentingProxy, recv_q, send_q

TARGET = "target"
SURVIVOR = "survivor"
PAYLOAD = "x" * 8_000_000


def emit(out, kind, **fields):
    rec = {"kind": kind, "t_wall": round(time.time(), 6), **fields}
    out.write(json.dumps(rec, default=str, sort_keys=True) + "\n")
    out.flush()
    print("REC " + json.dumps(rec, default=str, sort_keys=True)[:400], flush=True)


class FragCm(tx.CmScenario):
    """CmScenario with SSL disabled so every new connection is v3-framed."""

    def _dsn(self, port: int) -> str:
        if self.driver == "psycopg":
            return (
                f"host={self.args.host} port={port} user={self.args.user} "
                f"password={self.args.password} dbname={self.args.dbname} "
                f"connect_timeout=5 sslmode=disable"
            )
        return (
            f"postgresql://{self.args.user}:{self.args.password}"
            f"@{self.args.host}:{port}/{self.args.dbname}?sslmode=disable"
        )


class Holder(dict):
    def __init__(self, sc):
        super().__init__()
        self.sc = sc
        self.snaps: list[dict] = []

    def get(self, key, default=None):
        if key == "phase":
            sc = self.sc
            fd = sc.conn.pgconn.socket if sc.driver == "psycopg" else None
            self.snaps.append({
                "proxy": sc.proxy.snapshot(),
                "client_tx": tx.tx_name(sc.conn.info.transaction_status),
                "is_busy": bool(sc.conn.pgconn.is_busy()),
                "recv_q": recv_q(fd) if fd is not None else None,
                "send_q": send_q(fd) if fd is not None else None,
            })
        return super().get(key, default)


async def make(args, mode="off", pause=0.0, driver="psycopg"):
    a = argparse.Namespace(**vars(args))
    a.driver = driver
    sc = FragCm(a)
    await asyncio.wait_for(sc.setup(), 15)
    sc.holder = Holder(sc)
    sc.proxy.mode = mode
    sc.proxy.pause = pause
    sc.proxy.reset_stats()
    return sc


async def k_run(args, mode, reps=3, driver="psycopg", pause=0.15):
    rows = []
    for rep in range(1, reps + 1):
        sc = await make(args, mode, pause, driver)
        try:
            task = tx._PhasedTask(sc.target(), cut_after=None, holder=sc.holder)
            started = time.monotonic()
            res = await asyncio.wait_for(task, 20)
            secs = time.monotonic() - started
            row = {
                "driver": driver, "mode": mode, "pause": pause, "rep": rep,
                "outcome": "completed" if getattr(res, "ok", False) else "error",
                "yields": task.yields, "seconds": round(secs, 3),
                "proxy": sc.proxy.snapshot(),
            }
        finally:
            await close_quiet(sc.close())
        rows.append(row)
    return rows


async def observe(sc):
    client = sc._client_state()
    srv = await bound(sc._server_sample(), 2)
    judged = await bound(sc.judged(), 3)
    return client, (srv[1] if srv[0] == "ok" else {"error": srv[1]}), judged


async def calibrate(args, driver="psycopg"):
    pre = []
    post = []
    for _ in range(2):
        sc = await make(args, "off", 0.0, driver)
        try:
            pre.append(await bound(sc.judged(), 3))
        finally:
            await close_quiet(sc.close())
    for _ in range(2):
        sc = await make(args, "off", 0.0, driver)
        try:
            tr = await _await_target(sc.target(), None, 20)
            post.append(await bound(sc.judged(), 3))
        finally:
            await close_quiet(sc.close())
    return pre, post


def verdict(judged, pre, post, pre_ok, post_ok):
    if not pre_ok or not post_ok:
        return "Unknown"
    if judged == pre:
        return "Pre"
    if judged == post:
        return "Post"
    return "Neither"


async def cut_once(args, mode, k, pre, post, driver="psycopg"):
    sc = await make(args, mode, 0.15, driver)
    started = time.monotonic()
    try:
        tr, meta = await tx._await_phased(
            sc.target(), cut_after=k, timeout=20, holder=sc.holder, double=False
        )
        cut_secs = time.monotonic() - started
        # Stop fragmenting before the successor: the target's remainder has
        # already been drained by the driver's own cancellation handling.
        sc.proxy.mode = "off"
        await asyncio.sleep(0.05)
        client, server, judged = await observe(sc)
        pre_ok = pre[0][0] == "ok" and pre[0] == pre[1]
        post_ok = post[0][0] == "ok" and post[0] == post[1]
        v = verdict(judged, pre[0], post[0], pre_ok, post_ok)
        return {
            "driver": driver, "mode": mode, "k": k,
            "target": {"outcome": tr.outcome, "realized": tr.realized,
                       "yields": tr.yields, "error": tr.error,
                       "seconds": round(cut_secs, 3)},
            "phase": meta["cut_phase"],
            "cut_snapshots": sc.holder.snaps,
            "client_after_unwind": client,
            "server_after_unwind": server,
            "judged": judged,
            "verdict": v,
            "pre_eq_post": pre[0] == post[0],
            "proxy": sc.proxy.snapshot(),
            "seconds": round(time.monotonic() - started, 3),
        }
    finally:
        await close_quiet(sc.close())


async def evidence(args, mode):
    sc = await make(args, mode, 0.15)
    rec = {"mode": mode, "driver": "psycopg"}
    try:
        task = tx._PhasedTask(sc.target(), cut_after=None, holder=sc.holder)
        try:
            await asyncio.wait_for(sc.proxy.waiting.wait(), 5)
        except TimeoutError:
            rec["error"] = "no pause before target completed"
            return rec
        await asyncio.sleep(0.05)
        snap = sc.proxy.snapshot()
        fd = sc.conn.pgconn.socket
        rec.update({
            "proxy": snap,
            "frame": snap["current"],
            "partial": bool(snap["current"] and snap["current"]["sent"] < snap["current"]["len"]),
            "pause_active": snap["pause_active"],
            "task_suspended": not task.done(),
            "fut_waiter": repr(task._fut_waiter),
            "task_yields": task.yields,
            "recv_q": recv_q(fd),
            "send_q": send_q(fd),
            "client_is_busy": bool(sc.conn.pgconn.is_busy()),
            "client_tx": tx.tx_name(sc.conn.info.transaction_status),
        })
        res = await asyncio.wait_for(task, 20)
        rec["result_ok"] = bool(getattr(res, "ok", False))
        rec["k_total"] = task.yields
        return rec
    finally:
        await close_quiet(sc.close())


# ---------------------------------------------------------------- control
class Control:
    def __init__(self, args):
        self.args = args
        self.table = unique_table("dw_frag_ctrl")
        self.proxy = FragmentingProxy(args.host, args.port, delay=0.0, mode="off")
        self.port = None
        self.conn = None
        self.observer = None

    async def setup(self, mode):
        self.port = await self.proxy.start()
        a = self.args
        self.observer = await psycopg.AsyncConnection.connect(
            f"host={a.host} port={a.port} user={a.user} password={a.password} "
            f"dbname={a.dbname} sslmode=disable", autocommit=True)
        await self.observer.execute(
            f"CREATE TABLE {self.table} (k text primary key, v integer not null)")
        await self.observer.execute(
            f"INSERT INTO {self.table} VALUES ('{TARGET}', 1), ('next', 2)")
        self.conn = await psycopg.AsyncConnection.connect(
            f"host={a.host} port={self.port} user={a.user} password={a.password} "
            f"dbname={a.dbname} sslmode=disable", autocommit=True)
        self.proxy.mode = mode
        self.proxy.pause = 0.15
        self.proxy.reset_stats()

    async def target(self):
        cur = await self.conn.execute(
            f"SELECT v FROM {self.table} WHERE k = '{TARGET}'")
        return await cur.fetchone()

    async def successor(self):
        cur = await self.conn.execute(
            f"SELECT v FROM {self.table} WHERE k = 'next'")
        return await cur.fetchone()

    async def rows(self):
        cur = await self.observer.execute(
            f"SELECT k, v FROM {self.table} ORDER BY k")
        return await cur.fetchall()

    async def close(self):
        await close_quiet(self.conn.close()) if self.conn else None
        await close_quiet(self.observer.close()) if self.observer else None
        await close_quiet(self.proxy.close())


async def control_k(args, mode, reps=2):
    rows = []
    for rep in range(1, reps + 1):
        c = Control(args)
        await c.setup(mode)
        try:
            task = tx._PhasedTask(c.target(), cut_after=None,
                                  holder={"phase": "select"})
            started = time.monotonic()
            res = await asyncio.wait_for(task, 10)
            rows.append({"mode": mode, "rep": rep, "result": res,
                         "yields": task.yields,
                         "seconds": round(time.monotonic() - started, 3),
                         "frames": c.proxy.snapshot()["frames"]})
        finally:
            await c.close()
    return rows


async def control_cut(args, mode, ks):
    rows = []
    for k in ks:
        c = Control(args)
        await c.setup(mode)
        try:
            task = tx._PhasedTask(c.target(), cut_after=k,
                                  holder={"phase": "select"})
            try:
                res = await asyncio.wait_for(task, 10)
                outcome = {"outcome": "completed", "result": res}
            except asyncio.CancelledError:
                outcome = {"outcome": "cancelled"}
            except BaseException as exc:
                outcome = {"outcome": "error", "error": f"{type(exc).__name__}: {exc}"}
            c.proxy.mode = "off"
            await asyncio.sleep(0.03)
            succ = await bound(c.successor(), 3)
            rows_now = await bound(c.rows(), 3)
            rows.append({"mode": mode, "k": k, "target": outcome,
                         "successor": succ, "rows": rows_now,
                         "client_tx": tx.tx_name(c.conn.info.transaction_status),
                         "closed": c.conn.closed,
                         "control_ok": succ == ("ok", (2,)) and not c.conn.closed})
        finally:
            await c.close()
    return rows


# ------------------------------------------------------------ backpressure
class BPHolder(dict):
    def __init__(self, proxy, conn):
        super().__init__()
        self.proxy = proxy
        self.conn = conn
        self.snaps = []

    def get(self, key, default=None):
        if key == "phase":
            fd = self.conn.pgconn.socket
            self.snaps.append({
                "proxy": self.proxy.snapshot(),
                "send_q": send_q(fd),
                "client_tx": tx.tx_name(self.conn.info.transaction_status),
                "is_busy": bool(self.conn.pgconn.is_busy()),
            })
        return super().get(key, default)


async def bp_one(args, hold: bool):
    proxy = FragmentingProxy(args.host, args.port, delay=0.0, mode="off")
    port = await proxy.start()
    a = args
    conn = await psycopg.AsyncConnection.connect(
        f"host={a.host} port={port} user={a.user} password={a.password} "
        f"dbname={a.dbname} sslmode=disable", autocommit=True)
    fd = conn.pgconn.socket
    rec = {"hold": hold}
    try:
        if hold:
            proxy.arm_hold(65536)
        task = tx._PhasedTask(conn.execute("SELECT length(%s)", (PAYLOAD,)),
                              cut_after=None, holder={"phase": "send"})
        started = time.monotonic()
        if hold:
            await asyncio.wait_for(proxy.hold_reached.wait(), 5)
            await asyncio.sleep(0.3)
            rec.update({
                "suspended": not task.done(),
                "fut_waiter": repr(task._fut_waiter),
                "send_q": send_q(fd),
                "proxy_bytes_c2s": proxy.snapshot()["bytes_c2s"],
                "flush_pending": conn.pgconn.flush(),
                "client_tx": tx.tx_name(conn.info.transaction_status),
                "is_busy": bool(conn.pgconn.is_busy()),
            })
            proxy.release_hold()
        res = await asyncio.wait_for(task, 20)
        rec.update({"outcome": "completed", "result": await res.fetchone(),
                    "yields": task.yields,
                    "seconds": round(time.monotonic() - started, 3)})
        return rec
    finally:
        await close_quiet(conn.close())
        await close_quiet(proxy.close())


async def bp_cut(args):
    proxy = FragmentingProxy(args.host, args.port, delay=0.0, mode="off")
    port = await proxy.start()
    a = args
    conn = await psycopg.AsyncConnection.connect(
        f"host={a.host} port={port} user={a.user} password={a.password} "
        f"dbname={a.dbname} sslmode=disable", autocommit=True)
    fd = conn.pgconn.socket
    rec = {}
    try:
        proxy.reset_stats()
        proxy.arm_hold(65536)
        task = asyncio.create_task(
            conn.execute("SELECT length(%s)", (PAYLOAD,)))
        await asyncio.wait_for(proxy.hold_reached.wait(), 5)
        await asyncio.sleep(0.3)
        rec.update({
            "pre_cut_suspended": not task.done(),
            "pre_cut_send_q": send_q(fd),
            "pre_cut_proxy_bytes": proxy.snapshot()["bytes_c2s"],
            "pre_cut_proxy_writes": proxy.snapshot()["writes_c2s"],
            "pre_cut_flush": conn.pgconn.flush(),
            "pre_cut_tx": tx.tx_name(conn.info.transaction_status),
            "pre_cut_is_busy": bool(conn.pgconn.is_busy()),
        })
        # Direct cancel at the verified partial-write suspension, not a k-th
        # yield plan: the suspension itself is what this experiment verifies.
        task.cancel()
        rec["cut_phase"] = "send"
        rec["cut_requested"] = True
        proxy.release_hold()
        try:
            res = await asyncio.wait_for(task, 15)
            rec["outcome"] = {"outcome": "completed", "result": await res.fetchone()}
        except asyncio.CancelledError:
            rec["outcome"] = {"outcome": "cancelled"}
        except BaseException as exc:
            rec["outcome"] = {
                "outcome": "error",
                "error": f"{type(exc).__name__}: {exc}",
                "traceback_tail": traceback.format_exception(
                    type(exc), exc, exc.__traceback__)[-6:],
            }
        await asyncio.sleep(0.1)
        rec["after"] = {
            "client_tx": tx.tx_name(conn.info.transaction_status),
            "is_busy": bool(conn.pgconn.is_busy()),
            "closed": conn.closed,
            "send_q": send_q(fd),
        }
        try:
            cur = await asyncio.wait_for(conn.execute("SELECT 1"), 3)
            rec["successor"] = ("ok", await cur.fetchone())
        except BaseException as exc:
            rec["successor"] = ("err", f"{type(exc).__name__}: {exc}")
        rec["close_ok"] = None
        try:
            await asyncio.wait_for(conn.close(), 3)
            rec["close_ok"] = True
        except BaseException as exc:
            rec["close_ok"] = f"{type(exc).__name__}: {exc}"
        return rec
    finally:
        await close_quiet(proxy.close())


async def main():
    p = argparse.ArgumentParser()
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=32890)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--out", required=True)
    p.add_argument("--only", default="all", choices=["all", "bp"])
    args = p.parse_args()
    tx.DelayProxy = FragmentingProxy  # runtime only; txphase_case.py is untouched
    Path(args.out).mkdir(parents=True, exist_ok=True)
    jsonl = open(Path(args.out) / "records.jsonl", "w")
    emit(jsonl, "config", driver="psycopg", python=sys.version.split()[0],
         psycopg=psycopg.__version__, port=args.port,
         payload_bytes=len(PAYLOAD), pause=0.15)

    if args.only == "bp":
        for hold in (False, True):
            for rep in (1, 2):
                row = await bp_one(args, hold)
                row["rep"] = rep
                emit(jsonl, "bp", **row)
                print(f"BP hold={hold} rep={rep} yields={row.get('yields', '?')} "
                      f"suspended={row.get('suspended')} send_q={row.get('send_q')} "
                      f"proxy_bytes={row.get('proxy_bytes_c2s')} "
                      f"flush={row.get('flush_pending')} result={row.get('result')}", flush=True)
        for rep in (1, 2):
            row = await bp_cut(args)
            row["rep"] = rep
            emit(jsonl, "bp_cut", **row)
            print(f"BP-CUT rep={rep} pre_suspended={row['pre_cut_suspended']} "
                  f"send_q={row['pre_cut_send_q']} bytes={row['pre_cut_proxy_bytes']} "
                  f"outcome={row['outcome'].get('outcome')} error={row['outcome'].get('error')} "
                  f"after={row['after']} successor={row['successor']}", flush=True)
        emit(jsonl, "done")
        jsonl.close()
        return

    try:
        pre, post = await calibrate(args)
        emit(jsonl, "calibration", pre=pre, post=post,
             pre_ok=pre[0] == pre[1] and pre[0][0] == "ok",
             post_ok=post[0] == post[1] and post[0][0] == "ok",
             pre_eq_post=pre[0] == post[0])

        for mode in ("off", "boundary", "header", "payload"):
            rows = await k_run(args, mode, reps=3)
            emit(jsonl, "k", mode=mode, rows=rows)
            for row in rows:
                print(f"K mode={mode} rep={row['rep']} yields={row['yields']} "
                      f"secs={row['seconds']} frames={row['proxy']['frames']} "
                      f"pauses={row['proxy']['pause_count']}", flush=True)

        for mode in ("boundary", "header", "payload"):
            for k in (1, 2):
                row = await cut_once(args, mode, k, pre, post)
                emit(jsonl, "cut", **row)
                print(f"CUT mode={mode} k={k} phase={row['phase']} "
                      f"target={row['target']['outcome']} realized={row['target']['realized']} "
                      f"verdict={row['verdict']} cut={row['cut_snapshots'][0]['proxy']['current'] if row['cut_snapshots'] else None} "
                      f"server={row['server_after_unwind'].get('state')} "
                      f"client={row['client_after_unwind'].get('status')}", flush=True)

        # Full normal-path header ladder: every discovered suspension point.
        kmax = max(r["yields"] for r in await k_run(args, "header", reps=1))
        emit(jsonl, "ladder_plan", mode="header", kmax=kmax)
        for k in range(1, kmax + 1):
            row = await cut_once(args, "header", k, pre, post)
            emit(jsonl, "cut_full", **row)
            print(f"LADDER k={k} phase={row['phase']} "
                  f"target={row['target']['outcome']} realized={row['target']['realized']} "
                  f"verdict={row['verdict']} "
                  f"cut={(row['cut_snapshots'][0]['proxy']['current'] if row['cut_snapshots'] else None)}", flush=True)

        for mode in ("header", "payload"):
            for rep in (1, 2):
                ev = await evidence(args, mode)
                ev["rep"] = rep
                emit(jsonl, "evidence", **ev)
                print(f"EVIDENCE mode={mode} rep={rep} partial={ev.get('partial')} "
                      f"suspended={ev.get('task_suspended')} recv_q={ev.get('recv_q')} "
                      f"busy={ev.get('client_is_busy')} tx={ev.get('client_tx')} "
                      f"frame={ev.get('frame')}", flush=True)

        # Fixed version: header mode, entry cuts only.
        tx.install_psycopg_enter_fix()
        emit(jsonl, "fix", installed=True, driver="psycopg")
        pre_f, post_f = await calibrate(args)
        emit(jsonl, "calibration_fix", pre=pre_f, post=post_f)
        rows_f = await k_run(args, "header", reps=3)
        emit(jsonl, "k_fix", mode="header", rows=rows_f)
        for row in rows_f:
            print(f"K-FIX mode=header rep={row['rep']} yields={row['yields']} secs={row['seconds']}", flush=True)
        for k in (1, 2, 3):
            row = await cut_once(args, "header", k, pre_f, post_f)
            emit(jsonl, "cut_fix", **row)
            print(f"CUT-FIX k={k} phase={row['phase']} verdict={row['verdict']} "
                  f"server={row['server_after_unwind'].get('state')} "
                  f"client={row['client_after_unwind'].get('status')}", flush=True)

        # Positive control: psycopg plain SELECT cancellation (libpq drain).
        for mode in ("off", "boundary", "header", "payload"):
            rows = await control_k(args, mode, reps=2)
            emit(jsonl, "control_k", mode=mode, rows=rows)
            for row in rows:
                print(f"CTRL-K mode={mode} yields={row['yields']} secs={row['seconds']} "
                      f"result={row['result']}", flush=True)
        for mode in ("boundary", "header", "payload"):
            rows = await control_cut(args, mode, ks=(1, 2, 3))
            emit(jsonl, "control_cut", mode=mode, rows=rows)
            for row in rows:
                print(f"CTRL-CUT mode={mode} k={row['k']} {row['target']['outcome']} "
                      f"succ={row['successor']} ok={row['control_ok']}", flush=True)

        # Send-direction backpressure: K without hold, K/sample with hold, and
        # one cancellation at the verified partial-write suspension.
        for hold in (False, True):
            for rep in (1, 2):
                row = await bp_one(args, hold)
                row["rep"] = rep
                emit(jsonl, "bp", **row)
                print(f"BP hold={hold} rep={rep} yields={row.get('yields', '?')} "
                      f"suspended={row.get('suspended')} send_q={row.get('send_q')} "
                      f"proxy_bytes={row.get('proxy_bytes_c2s')} "
                      f"flush={row.get('flush_pending')} result={row.get('result')}", flush=True)
        for rep in (1, 2):
            row = await bp_cut(args)
            row["rep"] = rep
            emit(jsonl, "bp_cut", **row)
            print(f"BP-CUT rep={rep} suspended={row['pre_cut_suspended']} "
                  f"send_q={row['pre_cut_send_q']} bytes={row['pre_cut_proxy_bytes']} "
                  f"outcome={row['outcome'].get('outcome')} "
                  f"error={row['outcome'].get('error')} "
                  f"after={row['after']} successor={row['successor']}", flush=True)

        emit(jsonl, "done")
    finally:
        jsonl.close()


if __name__ == "__main__":
    asyncio.run(main())
