"""Precheck: which ordinary PostgreSQL failures are reachable per phase?

Small, direct-connection probe used to pick the *ordinary failure* arms of
`compare.py`.  It is not a judged experiment; it prints raw outcomes so the
harness can rely on errors that really come from the server, not from a
synthetic ErrorResponse.

Run from .../python/pg-mysql with PYTHONPATH=".:$REPO/research/python-retro".
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import traceback

import asyncpg
import psycopg

from cases.common import BOUND, bound, err_text, must_ok, unique_table


def sqlstate(exc: BaseException) -> str | None:
    return getattr(exc, "sqlstate", None) or getattr(exc, "pgcode", None)


def psycopg_conninfo(a) -> str:
    return (f"host={a.host} port={a.port} user={a.user} password={a.password} "
            f"dbname={a.dbname} connect_timeout=5")


def asyncpg_dsn(a) -> str:
    return f"postgresql://{a.user}:{a.password}@{a.host}:{a.port}/{a.dbname}"


async def make_table(observer, name: str) -> None:
    await observer.execute(
        f"CREATE TABLE {name} (k text PRIMARY KEY, v integer NOT NULL, "
        f"u integer, CONSTRAINT {name}_u UNIQUE (u) DEFERRABLE INITIALLY DEFERRED)"
    )


async def rows_of(observer, name: str, driver: str):
    if driver == "psycopg":
        cur = await observer.execute(f"SELECT k, v FROM {name} ORDER BY k")
        return [list(r) for r in await cur.fetchall()]
    return [list(r) for r in await observer.fetch(f"SELECT k, v FROM {name} ORDER BY k")]


def psycopg_state(conn, tx=None) -> dict:
    from psycopg.transaction import BaseTransaction
    out = {
        "status": conn.info.transaction_status.name,
        "closed": conn.closed,
        "num_transactions": getattr(conn, "_num_transactions", None),
    }
    if tx is not None:
        out["tx_status"] = getattr(tx.status, "name", repr(getattr(tx, "status", None)))
        out["tx_outer"] = getattr(tx, "_outer_transaction", None)
        out["tx_savepoint"] = getattr(tx, "_savepoint_name", None)
    return out


def asyncpg_state(conn, tx=None) -> dict:
    try:
        in_tx = conn.is_in_transaction()
        closed = conn.is_closed()
    except BaseException as exc:
        in_tx, closed = f"unavailable:{type(exc).__name__}", True
    return {
        "in_transaction": in_tx,
        "closed": closed,
        "top_xact_is_tx": getattr(conn, "_top_xact", None) is tx,
        "top_xact_set": getattr(conn, "_top_xact", None) is not None,
        "tx_state": getattr(getattr(tx, "_state", None), "name", None),
        "tx_nested": getattr(tx, "_nested", None),
    }


async def server_sample(observer, pid: int, driver: str) -> dict:
    q = ("SELECT state, backend_xid::text, left(query, 80) FROM pg_stat_activity "
         "WHERE pid = " + ("%s" if driver == "psycopg" else "$1"))
    try:
        if driver == "psycopg":
            cur = await observer.execute(q, (pid,))
            row = await cur.fetchone()
        else:
            row = await observer.fetchrow(q, pid)
    except BaseException as exc:
        return {"error": err_text(exc)}
    if row is None:
        return {"present": False}
    return {"state": row[0], "backend_xid": row[1], "query": row[2]}


async def probe_psycopg(a) -> None:
    dsn = psycopg_conninfo(a)
    observer = await psycopg.AsyncConnection.connect(dsn, autocommit=True)
    table = unique_table("dw_fc_pre_pg")
    await make_table(observer, table)
    out: dict = {"driver": "psycopg", "table": table}
    try:
        conn = await psycopg.AsyncConnection.connect(dsn, autocommit=True)
        pid = conn.info.backend_pid
        out["pid"] = pid

        # 1. server error in an outer CM body -> __aexit__ rollback
        tx = conn.transaction()
        inner = await tx.__aenter__()
        body_exc = None
        try:
            await conn.execute("SELECT 1/0")
        except BaseException as exc:
            body_exc = exc
            out["body_div_exc"] = err_text(exc)
            out["body_div_sqlstate"] = sqlstate(exc)
        out["body_after_error"] = psycopg_state(conn, inner)
        try:
            await tx.__aexit__(type(body_exc), body_exc, body_exc.__traceback__)
        except BaseException as exc:
            out["body_aexit_exc"] = err_text(exc)
        # sys.exc_info is unreliable here; call again with an explicit value
        out["body_after_aexit"] = psycopg_state(conn, inner)
        out["body_rows"] = await rows_of(observer, table, "psycopg")

        # 2. swallow the body error, then exit commit -> silent rollback?
        tx = conn.transaction()
        inner = await tx.__aenter__()
        await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('sw',7,7)")
        try:
            await conn.execute("SELECT 1/0")
        except BaseException as exc:
            out["swallow_exc"] = err_text(exc)
        out["swallow_before_aexit"] = psycopg_state(conn, inner)
        exc = None
        try:
            await tx.__aexit__(None, None, None)
            out["swallow_commit"] = "ok"
        except BaseException as e:
            out["swallow_commit"] = err_text(e)
            exc = e
        out["swallow_after_aexit"] = psycopg_state(conn, inner)
        out["swallow_rows"] = await rows_of(observer, table, "psycopg")

        # 3. deferred unique violation at COMMIT
        tx = conn.transaction()
        inner = await tx.__aenter__()
        await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('d1',1,99),('d2',2,99)")
        try:
            await tx.__aexit__(None, None, None)
            out["deferred_commit"] = "ok"
        except BaseException as e:
            out["deferred_commit"] = err_text(e)
            out["deferred_sqlstate"] = sqlstate(e)
        out["deferred_after_aexit"] = psycopg_state(conn, inner)
        out["deferred_rows"] = await rows_of(observer, table, "psycopg")
        # can the caller roll back / retry?
        try:
            await conn.rollback()
            out["deferred_rollback"] = "ok"
        except BaseException as e:
            out["deferred_rollback"] = err_text(e)
        out["deferred_after_rollback"] = psycopg_state(conn)
        tx2 = conn.transaction()
        try:
            await tx2.__aenter__()
            await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('r1',5,5)")
            await tx2.__aexit__(None, None, None)
            out["deferred_retry"] = "ok"
        except BaseException as e:
            out["deferred_retry"] = err_text(e)
        out["deferred_retry_rows"] = await rows_of(observer, table, "psycopg")

        # 4. failed SAVEPOINT entry inside an aborted transaction
        await conn.execute("BEGIN")
        try:
            await conn.execute("SELECT 1/0")
        except BaseException as e:
            out["aborted_outer_exc"] = err_text(e)
        out["aborted_before_entry"] = psycopg_state(conn)
        ntx = conn.transaction()
        try:
            ninner = await ntx.__aenter__()
            out["nested_entry"] = "ok"
        except BaseException as e:
            out["nested_entry"] = err_text(e)
            out["nested_entry_sqlstate"] = sqlstate(e)
        out["nested_after_failed_entry"] = psycopg_state(conn, locals().get("ninner"))
        try:
            await conn.rollback()
            out["nested_rollback"] = "ok"
        except BaseException as e:
            out["nested_rollback"] = err_text(e)
        out["nested_after_rollback"] = psycopg_state(conn)
        try:
            await conn.execute("ROLLBACK")
            out["nested_raw_rollback"] = "ok"
        except BaseException as e:
            out["nested_raw_rollback"] = err_text(e)
        out["nested_after_raw"] = psycopg_state(conn)
        tx3 = conn.transaction()
        try:
            await tx3.__aenter__()
            await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('n1',9,9)")
            await tx3.__aexit__(None, None, None)
            out["nested_retry"] = "ok"
        except BaseException as e:
            out["nested_retry"] = err_text(e)
        out["nested_retry_rows"] = await rows_of(observer, table, "psycopg")

        # 5. server terminates the session, then CM entry
        await observer.execute("SELECT pg_terminate_backend(%s)", (pid,))
        await asyncio.sleep(0.1)
        out["terminated_pre_state"] = psycopg_state(conn)
        try:
            term_tx = conn.transaction()
            await term_tx.__aenter__()
            out["terminated_entry"] = "ok"
        except BaseException as e:
            out["terminated_entry"] = err_text(e)
            out["terminated_sqlstate"] = sqlstate(e)
            out["terminated_tb_last"] = traceback.format_exc().strip().splitlines()[-4:]
        out["terminated_after"] = psycopg_state(conn)
        await conn.close()
    finally:
        await observer.execute(f"DROP TABLE IF EXISTS {table}")
        await observer.close()
    print("PRECHECK " + json.dumps(out, default=str, sort_keys=True), flush=True)


async def probe_asyncpg(a) -> None:
    dsn = asyncpg_dsn(a)
    observer = await asyncpg.connect(dsn)
    table = unique_table("dw_fc_pre_apg")
    await make_table(observer, table)
    out: dict = {"driver": "asyncpg", "table": table}
    try:
        conn = await asyncpg.connect(dsn)
        pid = conn.get_server_pid()
        out["pid"] = pid

        # 1. server error in CM body -> __aexit__ rollback
        tx = conn.transaction()
        await tx.__aenter__()
        body_exc = None
        try:
            await conn.execute("SELECT 1/0")
        except BaseException as exc:
            body_exc = exc
            out["body_div_exc"] = err_text(exc)
            out["body_div_sqlstate"] = sqlstate(exc)
        out["body_after_error"] = asyncpg_state(conn, tx)
        try:
            await tx.__aexit__(type(body_exc), body_exc, body_exc.__traceback__)
        except BaseException as exc:
            out["body_aexit_exc"] = err_text(exc)
        out["body_after_aexit"] = asyncpg_state(conn, tx)
        out["body_rows"] = await rows_of(observer, table, "asyncpg")

        # 2. swallow error, normal exit -> commit of an aborted tx
        tx = conn.transaction()
        await tx.__aenter__()
        await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('sw',7,7)")
        try:
            await conn.execute("SELECT 1/0")
        except BaseException as exc:
            out["swallow_exc"] = err_text(exc)
        out["swallow_before_aexit"] = asyncpg_state(conn, tx)
        try:
            await tx.__aexit__(None, None, None)
            out["swallow_commit"] = "ok"
        except BaseException as e:
            out["swallow_commit"] = err_text(e)
        out["swallow_after_aexit"] = asyncpg_state(conn, tx)
        out["swallow_rows"] = await rows_of(observer, table, "asyncpg")

        # 3. deferred unique violation at COMMIT
        tx = conn.transaction()
        await tx.__aenter__()
        await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('d1',1,99),('d2',2,99)")
        try:
            await tx.__aexit__(None, None, None)
            out["deferred_commit"] = "ok"
        except BaseException as e:
            out["deferred_commit"] = err_text(e)
            out["deferred_sqlstate"] = sqlstate(e)
        out["deferred_after_aexit"] = asyncpg_state(conn, tx)
        out["deferred_rows"] = await rows_of(observer, table, "asyncpg")
        try:
            await conn.execute("ROLLBACK")
            out["deferred_rollback"] = "ok"
        except BaseException as e:
            out["deferred_rollback"] = err_text(e)
        out["deferred_after_rollback"] = asyncpg_state(conn)
        tx2 = conn.transaction()
        try:
            await tx2.__aenter__()
            await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('r1',5,5)")
            await tx2.__aexit__(None, None, None)
            out["deferred_retry"] = "ok"
        except BaseException as e:
            out["deferred_retry"] = err_text(e)
        out["deferred_retry_rows"] = await rows_of(observer, table, "asyncpg")

        # 4. failed SAVEPOINT entry inside an aborted managed outer tx
        outer = conn.transaction()
        await outer.__aenter__()
        try:
            await conn.execute("SELECT 1/0")
        except BaseException as e:
            out["aborted_outer_exc"] = err_text(e)
        out["aborted_before_entry"] = asyncpg_state(conn, outer)
        ntx = conn.transaction()
        try:
            await ntx.__aenter__()
            out["nested_entry"] = "ok"
        except BaseException as e:
            out["nested_entry"] = err_text(e)
            out["nested_entry_sqlstate"] = sqlstate(e)
        out["nested_after_failed_entry"] = asyncpg_state(conn, ntx)
        try:
            await ntx.rollback()
            out["nested_rollback"] = "ok"
        except BaseException as e:
            out["nested_rollback"] = err_text(e)
        try:
            await outer.__aexit__(None, None, None)
            out["outer_commit_after_aborted"] = "ok"
        except BaseException as e:
            out["outer_commit_after_aborted"] = err_text(e)
        out["outer_after_aexit"] = asyncpg_state(conn, outer)
        out["aborted_rows"] = await rows_of(observer, table, "asyncpg")
        try:
            await conn.execute("ROLLBACK")
            out["outer_raw_rollback"] = "ok"
        except BaseException as e:
            out["outer_raw_rollback"] = err_text(e)
        tx3 = conn.transaction()
        try:
            await tx3.__aenter__()
            await conn.execute(f"INSERT INTO {table} (k,v,u) VALUES ('n1',9,9)")
            await tx3.__aexit__(None, None, None)
            out["nested_retry"] = "ok"
        except BaseException as e:
            out["nested_retry"] = err_text(e)
        out["nested_retry_rows"] = await rows_of(observer, table, "asyncpg")

        # 5. server terminates the session, then CM entry
        await observer.execute("SELECT pg_terminate_backend($1)", pid)
        await asyncio.sleep(0.1)
        out["terminated_pre_state"] = asyncpg_state(conn)
        try:
            term_tx = conn.transaction()
            await term_tx.__aenter__()
            out["terminated_entry"] = "ok"
        except BaseException as e:
            out["terminated_entry"] = err_text(e)
            out["terminated_sqlstate"] = sqlstate(e)
            out["terminated_tb_last"] = traceback.format_exc().strip().splitlines()[-3:]
        out["terminated_after"] = asyncpg_state(conn)
        await conn.close()
    finally:
        await observer.execute(f"DROP TABLE IF EXISTS {table}")
        await observer.close()
    print("PRECHECK " + json.dumps(out, default=str, sort_keys=True), flush=True)


async def amain(a) -> None:
    if a.driver in ("psycopg", "both"):
        await probe_psycopg(a)
    if a.driver in ("asyncpg", "both"):
        await probe_asyncpg(a)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=32870)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--driver", default="both",
                   choices=["psycopg", "asyncpg", "both"])
    asyncio.run(amain(p.parse_args()))


if __name__ == "__main__":
    main()
