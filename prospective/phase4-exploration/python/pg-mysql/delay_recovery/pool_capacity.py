"""psycopg_pool: capacity and successor cost while an entry-cut lease is held.

The proxy delays only BEGIN replies.  A one-slot pool is used so "successor
availability" is observable as pool capacity, not inferred from a new direct
connection.  No-recovery, explicit close, and the fail-closed fix are compared.
"""
from __future__ import annotations
import argparse, asyncio, contextlib, json, sys, time
import psycopg
from psycopg_pool import AsyncConnectionPool
from cases.common import add_db_args, close_quiet, err_text, unique_table
from cases.txphase_case import install_psycopg_enter_fix
from delay_recovery.psycopg_delay import OneWayDelayProxy

async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--delay", type=float, default=5.0)
    ap.add_argument("--modes", default="none,close,fix")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for mode in args.modes.split(","):
            mode = mode.strip()
            if mode == "fix":
                install_psycopg_enter_fix()
            proxy = OneWayDelayProxy(args.host, args.port, delay=0.05)
            port = await proxy.start()
            table = unique_table("dw_poolcap")
            obs = await psycopg.AsyncConnection.connect(
                f"host={args.host} port={args.port} user={args.user} "
                f"password={args.password} dbname={args.dbname}",
                autocommit=True,
            )
            await obs.execute(f"CREATE TABLE {table} (k text PRIMARY KEY, v int)")
            pool = AsyncConnectionPool(
                f"host={args.host} port={port} user={args.user} "
                f"password={args.password} dbname={args.dbname}",
                min_size=1, max_size=1, open=False,
                timeout=args.timeout,
                kwargs={"autocommit": True},
            )
            rec = {"mode": mode, "delay": args.delay, "table": table}
            try:
                await pool.open(wait=True)
                # Pool open/health checks used 50 ms; only the target window
                # gets the long one-way response delay.
                proxy.delay = args.delay
                rec["pool_open"] = pool.get_stats()
                conn = await pool.getconn(timeout=args.timeout)
                pid = conn.info.backend_pid
                rec["pid"] = pid
                tx = conn.transaction()
                task = asyncio.create_task(tx.__aenter__())
                deadline = time.monotonic() + args.timeout
                begin = None
                while time.monotonic() < deadline:
                    cur = await obs.execute(
                        "SELECT state, (xact_start IS NOT NULL)::text, pid "
                        "FROM pg_stat_activity WHERE pid = %s", (pid,))
                    row = await cur.fetchone()
                    if row and (row[0] == "idle in transaction" or row[1] == "true"):
                        begin = {"state": row[0], "in_xact": row[1], "pid": row[2]}
                        break
                    await asyncio.sleep(0.005)
                rec["begin"] = begin
                t_cut = time.monotonic()
                task.cancel()
                with contextlib.suppress(BaseException):
                    await task
                t_return = time.monotonic()
                rec["cancel_to_return"] = round(t_return - t_cut, 4)
                rec["pool_after_cut"] = pool.get_stats()
                # Capacity while the old lease is still held: do not issue a
                # timed-out acquire here, because a timed-out waiter can remain
                # queued and contaminate the successor measurement below.
                rec["pool_while_held"] = pool.get_stats()
                # Recovery / release.
                rec["recovery"] = {}
                t_rec = time.monotonic()
                if mode == "close":
                    with contextlib.suppress(BaseException):
                        await conn.close()
                    rec["recovery"]["close_seconds"] = round(time.monotonic()-t_rec, 4)
                elif mode == "fix":
                    rec["recovery"]["kind"] = "inside_cancel"
                else:
                    rec["recovery"]["kind"] = "none"
                t_put = time.monotonic()
                try:
                    await pool.putconn(conn)
                    rec["recovery"]["putconn"] = "ok"
                except BaseException as exc:
                    rec["recovery"]["putconn"] = err_text(exc)
                rec["recovery"]["putconn_seconds"] = round(time.monotonic()-t_put, 4)
                # Wait for a usable successor; the pool may create a new
                # connection through the delayed proxy.
                t_succ = time.monotonic()
                try:
                    nxt = await pool.getconn(timeout=args.timeout)
                    rec["successor"] = {"ok": True, "pid": nxt.info.backend_pid,
                                        "seconds": round(time.monotonic()-t_succ, 4)}
                    cur = await nxt.execute(f"INSERT INTO {table} (k,v) VALUES ('succ',7)")
                    rec["successor"]["write"] = cur.statusmessage
                    await pool.putconn(nxt)
                except BaseException as exc:
                    rec["successor"] = {"ok": False, "error": err_text(exc),
                                        "seconds": round(time.monotonic()-t_succ, 4)}
                cur = await obs.execute(f"SELECT k,v FROM {table} ORDER BY k")
                rec["rows"] = [list(r) for r in await cur.fetchall()]
                rec["pool_end"] = pool.get_stats()
                rec["total_seconds"] = round(time.monotonic()-t_cut, 4)
                if rec["successor"].get("ok") is False:
                    failures += 1
            except BaseException as exc:
                failures += 1
                rec["error"] = err_text(exc)
                rec["trace"] = __import__("traceback").format_exc()[-1200:]
            finally:
                with contextlib.suppress(BaseException):
                    await pool.close()
                with contextlib.suppress(BaseException):
                    await obs.execute(f"DROP TABLE IF EXISTS {table}")
                await close_quiet(obs.close())
                await close_quiet(proxy.close())
            print("POOLCAP " + json.dumps(rec, default=str, sort_keys=True),
                  file=out, flush=True)
            print(f"POOLCAP-SUMMARY mode={mode} begin={bool(rec.get('begin'))} "
                  f"c2r={rec.get('cancel_to_return')} "
                  f"held={rec.get('pool_while_held')} "
                  f"next_held_ok={((rec.get('next_while_held') or {}).get('ok'))} "
                  f"succ={((rec.get('successor') or {}).get('ok'))} "
                  f"succ_s={((rec.get('successor') or {}).get('seconds'))} "
                  f"end={rec.get('pool_end')} err={rec.get('error')}", flush=True)
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0

if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
