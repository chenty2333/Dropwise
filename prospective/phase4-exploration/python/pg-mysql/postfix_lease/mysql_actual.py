"""Verify the actual mysql-connector aio cancellation patch (e7a02b6).

This is deliberately small: it is not the round-5 delay/re-cancellation matrix.
It checks the contract the actual patch claims: a cancelled read poisons the
socket and later operations fail explicitly instead of consuming a stale reply.
The earlier ``fix/patched`` tree is a different, older direct-close experiment
and is not used here.

Modes: normal, entry, nested, post_commit.  A fresh tested connection and a
fresh table are used for every cell; the observer is an independent sync
connection.  The tested connection goes through a 200 ms DelayProxy only in
the cancellation modes.
"""
from __future__ import annotations

import argparse
import asyncio
import contextlib
import inspect
import json
import os
import sys
import time

import mysql.connector
import mysql.connector.aio as aio

from cases.common import DelayProxy, close_quiet

TABLE_SEQ = 0


def package_info() -> dict:
    """Facts about the mysql-connector package actually imported."""
    import mysql.connector.aio.network as net

    src = inspect.getsource(net)
    return {
        "version": getattr(mysql.connector, "__version__", None),
        "file": getattr(mysql.connector, "__file__", None),
        "network_file": getattr(net, "__file__", None),
        "has_is_poisoned": "_is_poisoned" in src,
        "has_connection_not_available": "MySQL Connection not available" in src,
        "has_close_reason": "_close_reason" in src,
    }


def table_name() -> str:
    global TABLE_SEQ
    TABLE_SEQ += 1
    return f"dw_mysql_actual_{os.getpid()}_{TABLE_SEQ}"


def observer_connect(args):
    return mysql.connector.connect(
        host=args.host, port=args.port, user=args.user,
        password=args.password, database=args.dbname, connection_timeout=5,
    )


def obs_rows(obs, table):
    cur = obs.cursor()
    try:
        cur.execute(f"SELECT k, v FROM {table} ORDER BY k")
        return [[k, v] for k, v in cur.fetchall()]
    finally:
        cur.close()


def obs_trx(obs, connection_id):
    cur = obs.cursor()
    try:
        cur.execute(
            "SELECT trx_id, trx_state FROM information_schema.innodb_trx "
            "WHERE trx_mysql_thread_id = %s", (connection_id,))
        return [list(r) for r in cur.fetchall()]
    finally:
        cur.close()


def err(exc: BaseException) -> str:
    return f"{type(exc).__name__}: {exc}"


async def exec_conn(conn, sql, params=()):
    cur = await conn.cursor()
    try:
        await cur.execute(sql, params)
        if cur.with_rows:
            await cur.fetchall()
        return cur.rowcount
    finally:
        with contextlib.suppress(BaseException):
            await cur.close()


def state(conn) -> dict:
    sock = getattr(conn, "_socket", None)
    return {
        "connection_id": getattr(conn, "connection_id", None),
        "poisoned": getattr(sock, "_is_poisoned", None),
        "socket_open": getattr(sock, "is_connected", lambda: None)(),
        "in_transaction": getattr(conn, "in_transaction", None),
        "unread_result": getattr(conn, "unread_result", None),
    }


async def connect(args, port):
    return await aio.connect(
        host=args.host, port=port, user=args.user, password=args.password,
        database=args.dbname, autocommit=True, connection_timeout=5,
    )


async def recovery(conn, table) -> dict:
    out = {}
    for name in ("rollback", "reset_session"):
        try:
            await getattr(conn, name)()
            out[name] = "ok"
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            out[name] = err(exc)
    try:
        out["successor"] = ["ok", await exec_conn(
            conn, f"INSERT INTO {table} (k, v) VALUES (%s, %s)",
            ("successor", 7))]
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        out["successor"] = ["err", err(exc)]
    try:
        out["connected"] = await conn.is_connected()
    except BaseException as exc:
        out["connected"] = err(exc)
    out["state"] = state(conn)
    return out


async def run_cell(args, mode: str) -> dict:
    table = table_name()
    obs = observer_connect(args)
    proxy = None
    conn = None
    rec = {"mode": mode, "table": table, "package": package_info()}
    started = time.monotonic()
    try:
        cur = obs.cursor()
        cur.execute(f"CREATE TABLE {table} (k VARCHAR(32) PRIMARY KEY, "
                    "v INTEGER NOT NULL)")
        obs.commit()
        cur.close()
        port = args.port
        if mode != "normal":
            proxy = DelayProxy(args.host, args.port, delay=args.delay)
            port = await proxy.start()
        conn = await connect(args, port)
        rec["connection_id"] = getattr(conn, "connection_id", None)
        if mode == "normal":
            await conn.start_transaction()
            await exec_conn(conn, f"INSERT INTO {table} (k, v) VALUES (%s, %s)",
                            ("target", 1))
            await conn.commit()
            rec["target"] = "completed"
            rec["rows"] = obs_rows(obs, table)
        elif mode in ("entry", "nested", "post_commit"):
            if mode == "post_commit":
                await conn.start_transaction()
                await exec_conn(
                    conn, f"INSERT INTO {table} (k, v) VALUES (%s, %s)",
                    ("target", 1))
                task = asyncio.create_task(conn.commit())
            else:
                task = asyncio.create_task(conn.start_transaction())
                if mode == "nested":
                    await task
                    task = None
                    await exec_conn(
                        conn, f"INSERT INTO {table} (k, v) VALUES (%s, %s)",
                        ("outer", 2))
                    task = asyncio.create_task(
                        exec_conn(conn, "SAVEPOINT dw_actual_sp"))
            await asyncio.sleep(args.cut_delay)
            rec["trx_before_cancel"] = obs_trx(obs, rec["connection_id"])
            rec["state_before_cancel"] = state(conn)
            task.cancel()
            try:
                await task
                rec["cut"] = "completed"
            except asyncio.CancelledError:
                rec["cut"] = "cancelled"
            except BaseException as exc:
                rec["cut"] = err(exc)
            rec["state_after_cancel"] = state(conn)
            rec["trx_after_cancel"] = obs_trx(obs, rec["connection_id"])
            rec["rows_after_cancel"] = obs_rows(obs, table)
            rec["recovery"] = await recovery(conn, table)
            rec["rows_final"] = obs_rows(obs, table)
        else:
            raise RuntimeError(f"unknown mode {mode}")
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        rec["error"] = err(exc)
        rec["trace"] = __import__("traceback").format_exc()[-1200:]
    finally:
        if conn is not None:
            await close_quiet(conn.close())
        if proxy is not None:
            await close_quiet(proxy.close())
        with contextlib.suppress(BaseException):
            cur = obs.cursor()
            cur.execute(f"DROP TABLE IF EXISTS {table}")
            obs.commit()
            cur.close()
        obs.close()
        rec["seconds"] = round(time.monotonic() - started, 3)
    return rec


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=32922)
    ap.add_argument("--user", default="root")
    ap.add_argument("--password", default="dw")
    ap.add_argument("--dbname", default="dw")
    ap.add_argument("--modes", default="normal,entry,nested,post_commit")
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--delay", type=float, default=0.2)
    ap.add_argument("--cut-delay", type=float, default=0.1)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for mode in args.modes.split(","):
                rec = await run_cell(args, mode.strip())
                rec["rep"] = rep
                if rec.get("error"):
                    failures += 1
                print("MYSQL-ACTUAL " + json.dumps(
                    rec, default=str, sort_keys=True), file=out, flush=True)
                print(
                    f"MYSQL-SUMMARY mode={mode} rep={rep} "
                    f"cut={rec.get('cut')} "
                    f"state_after={rec.get('state_after_cancel')} "
                    f"rows_after={rec.get('rows_after_cancel')} "
                    f"recovery={rec.get('recovery')} "
                    f"rows_final={rec.get('rows_final')} "
                    f"seconds={rec.get('seconds')}", flush=True)
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
