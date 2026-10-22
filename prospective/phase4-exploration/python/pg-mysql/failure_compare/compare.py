"""Ordinary failure vs one-sided interruption at matching transaction phases.

Question (group 4): at the *same semantic phase* of an official transaction
context manager, is the state left by a Python `Task.cancel()` different from
the state left by a real server-side error/refusal, and does the difference
change what a successor may assume?

Arms (per driver):
  normal                        control: enter / INSERT / commit
  cancel_entry                  real asyncio Task.cancel, first entry suspension
  cancel_body                   real asyncio Task.cancel, first body suspension
  cancel_exit_commit            real asyncio Task.cancel, first commit suspension
  server_body_div               real 22012 division_by_zero in the body
  server_body_unique            real 23505 unique violation in the body
  server_body_swallowed_commit  body error swallowed, then normal exit -> COMMIT
  server_exit_commit_deferred   real 23505 from a DEFERRABLE UNIQUE at COMMIT
  server_entry_savepoint        real 25P02 from SAVEPOINT entry while aborted
  server_entry_terminated       real admin termination of the backend, then entry

No ErrorResponse is injected into a healthy connection.  Every server error in
this file is produced by PostgreSQL executing the statement shown.  The only
injected mechanism is latency: the tested handle is wrapped in the read-only
retro `DelayProxy` (50 ms per server->client chunk) so a cancellation is
delivered while the reply is genuinely in flight; the independent observer is
direct and never uses the proxy.

Cancellation reuses `_PhasedTask` / `_await_phased` from `cases/txphase_case.py`
(read-only import), so the mechanism is identical to the round-4 phase probe.
Phase labels are recorded inside `cancel()` while the target is suspended.

Output: one JSON object per trial (JSONL), plus a calibration file.  A trial
whose cut was not realised is recorded as unrealized and is not interpreted.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import importlib.metadata
import json
import os
import sys
import time
import traceback

import asyncpg
import psycopg

from cases.common import (  # noqa: E402  (also puts research/python-retro on sys.path)
    BOUND,
    DELAY,
    DelayProxy,
    bound,
    close_quiet,
    err_text,
    must_ok,
    sanitize,
    unique_table,
)
from cases.txphase_case import _PhasedTask, _await_phased  # noqa: E402

TARGET_ROW = "target"
RETRY_ROW = "retry"
RETRY2_ROW = "retry2"
SUCCESSOR_VALUE = 5

SETTLE = 0.15          # after an unwind, before the independent server sample
TRIAL_TIMEOUT = 10.0
CANCEL_TIMEOUT = 8.0


# ---------------------------------------------------------------------------
# small helpers
# ---------------------------------------------------------------------------
def exc_info(exc: BaseException) -> dict:
    return {
        "class": type(exc).__name__,
        "sqlstate": getattr(exc, "sqlstate", None) or getattr(exc, "pgcode", None),
        "text": str(exc)[:400],
    }


def tx_status_name(value) -> str:
    try:
        return value.name
    except AttributeError:
        return str(value)


class Driver:
    """Per-driver connection/query/state adapters, kept deliberately small."""

    def __init__(self, name: str, args) -> None:
        self.name = name
        self.args = args

    # -- connections ---------------------------------------------------
    def _conninfo(self, port: int) -> str:
        a = self.args
        if self.name == "psycopg":
            return (f"host={a.host} port={port} user={a.user} "
                    f"password={a.password} dbname={a.dbname} connect_timeout=5")
        return (f"postgresql://{a.user}:{a.password}@{a.host}:{port}/{a.dbname}")

    async def connect(self, port: int, *, autocommit: bool = False):
        if self.name == "psycopg":
            return await psycopg.AsyncConnection.connect(
                self._conninfo(port), autocommit=autocommit
            )
        return await asyncpg.connect(self._conninfo(port), timeout=BOUND)

    async def pid(self, conn) -> int:
        if self.name == "psycopg":
            return conn.info.backend_pid
        return conn.get_server_pid()

    def closed(self, conn) -> bool:
        return network_closed(conn)

    # -- statements ----------------------------------------------------
    async def execute(self, conn, sql: str, params=None):
        if self.name == "psycopg":
            args = () if params is None else (params if isinstance(params, tuple) else (params,))
            cur = await conn.execute(sql, args)
            return cur.statusmessage
        if params is None:
            return await conn.execute(sql)
        if isinstance(params, (list, tuple)):
            return await conn.execute(sql, *params)
        return await conn.execute(sql, params)

    async def fetchval(self, conn, sql: str, params=None):
        if self.name == "psycopg":
            args = () if params is None else (params if isinstance(params, tuple) else (params,))
            cur = await conn.execute(sql, args)
            row = await cur.fetchone()
            return None if row is None else row[0]
        if params is None:
            return await conn.fetchval(sql)
        if isinstance(params, (list, tuple)):
            return await conn.fetchval(sql, *params)
        return await conn.fetchval(sql, params)

    async def observer_rows(self, observer, table: str) -> list:
        if self.name == "psycopg":
            cur = await observer.execute(
                f"SELECT k, v FROM {table} ORDER BY k"
            )
            return [[r[0], r[1]] for r in await cur.fetchall()]
        rows = await observer.fetch(f"SELECT k, v FROM {table} ORDER BY k")
        return [[r["k"], r["v"]] for r in rows]

    async def server_sample(self, observer) -> dict:
        if self.pid_value is None:
            return {"present": False}
        if self.name == "psycopg":
            cur = await observer.execute(
                "SELECT state, backend_xid::text, left(query, 100), wait_event_type "
                "FROM pg_stat_activity WHERE pid = %s", (self.pid_value,)
            )
            row = await cur.fetchone()
        else:
            rec = await observer.fetchrow(
                "SELECT state, backend_xid::text, left(query, 100) AS query, "
                "wait_event_type FROM pg_stat_activity WHERE pid = $1",
                self.pid_value,
            )
            row = None if rec is None else tuple(rec)
        if row is None:
            return {"pid": self.pid_value, "present": False}
        return {"pid": self.pid_value, "present": True, "state": row[0],
                "backend_xid": row[1], "query": row[2], "wait": row[3]}

    def client_state(self, conn, txobj=None) -> dict:
        if self.name == "psycopg":
            out = {
                "kind": "psycopg",
                "status": tx_status_name(conn.info.transaction_status),
                "closed": conn.closed,
                "num_transactions": getattr(conn, "_num_transactions", None),
            }
            if txobj is not None:
                out["tx_status"] = tx_status_name(getattr(txobj, "status", None))
                out["tx_outer"] = getattr(txobj, "_outer_transaction", None)
            return out
        try:
            in_tx = conn.is_in_transaction()
        except BaseException as exc:
            in_tx = f"unavailable:{type(exc).__name__}"
        top = getattr(conn, "_top_xact", None)
        out = {
            "kind": "asyncpg",
            "in_transaction": in_tx,
            "closed": network_closed(conn),
            "top_xact_set": top is not None,
            "top_xact_is_object": (top is txobj) if txobj is not None else False,
        }
        if top is not None:
            out["top_xact_state"] = getattr(getattr(top, "_state", None), "name", None)
        if txobj is not None:
            out["tx_state"] = getattr(getattr(txobj, "_state", None), "name", None)
            out["tx_nested"] = getattr(txobj, "_nested", None)
        return out


def network_closed(conn) -> bool:
    try:
        return bool(conn.is_closed())
    except BaseException:
        pass
    try:
        return bool(conn.closed)
    except BaseException:
        return True


# ---------------------------------------------------------------------------
# one scenario = one table + independent observer + tested handle via proxy
# ---------------------------------------------------------------------------
class Scenario:
    def __init__(self, driver: Driver, args) -> None:
        self.d = driver
        self.args = args
        self.table = unique_table(f"dw_fc_{driver.name}")
        self.proxy: DelayProxy | None = None
        self.proxy_port: int | None = None
        self.conn = None
        self.observer = None
        self.txobj = None
        self.pid_value: int | None = None
        self.events: list[dict] = []
        self.d.pid_value = None
        self.trial_id = ""

    # -- setup/teardown ------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=a.delay)
        self.proxy_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        self.observer = must_ok(
            "observer connect",
            await bound(self.d.connect(a.port, autocommit=(self.d.name == "psycopg"))),
        )
        await self._observer_exec(
            f"CREATE TABLE {self.table} ("
            f"k text PRIMARY KEY, v integer NOT NULL, u integer, "
            f"CONSTRAINT {self.table}_u UNIQUE (u) DEFERRABLE INITIALLY DEFERRED)"
        )
        self.conn = must_ok(
            "tested connect",
            await bound(self.d.connect(self.proxy_port,
                                      autocommit=(self.d.name == "psycopg"))),
        )
        self.pid_value = await self.d.pid(self.conn)
        self.d.pid_value = self.pid_value
        self._event("setup", pid=self.pid_value)
        await self._preflight()

    async def _preflight(self) -> None:
        out: dict = {}
        holder: dict = {"phase": "preflight"}
        try:
            await asyncio.wait_for(
                self._cm_block(holder, out, lambda: self._insert(TARGET_ROW, 1, 1)),
                TRIAL_TIMEOUT,
            )
        except BaseException as exc:
            raise RuntimeError(f"preflight CM failed: {err_text(exc)}\n{out!r}")
        rows = await self._observer_rows()
        if rows != [[TARGET_ROW, 1]]:
            raise RuntimeError(f"preflight not observed: {rows!r}")
        await self._observer_exec(f"DELETE FROM {self.table}")
        rows = await self._observer_rows()
        if rows != []:
            raise RuntimeError(f"preflight cleanup not observed: {rows!r}")
        self._event("preflight_ok", rows=rows)

    async def close(self) -> None:
        if self.observer is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(
                    self._observer_exec(f"DROP TABLE IF EXISTS {self.table}"), BOUND
                )
        for target in (self.conn, self.observer):
            if target is None:
                continue
            try:
                await close_quiet(target.close())
            except BaseException:
                pass
        if self.proxy is not None:
            with contextlib.suppress(BaseException):
                await close_quiet(self.proxy.close())

    # -- observation helpers ------------------------------------------
    def _event(self, name: str, **fields) -> None:
        rec = {"seq": len(self.events) + 1,
               "t": round(time.monotonic(), 6), "event": name}
        rec.update(fields)
        self.events.append(rec)

    async def _observer_exec(self, sql: str, params=None):
        return await self.d.execute(self.observer, sql, params)

    async def _observer_rows(self) -> list:
        return await self.d.observer_rows(self.observer, self.table)

    async def _server_sample(self) -> dict:
        try:
            return await asyncio.wait_for(self.d.server_sample(self.observer), BOUND)
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def _insert(self, row: str, value: int, u: int) -> str:
        sql = (f"INSERT INTO {self.table} (k, v, u) VALUES "
               + ("(%s, %s, %s)" if self.d.name == "psycopg" else "($1, $2, $3)")
               + " ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v, u = EXCLUDED.u")
        return await self.d.execute(self.conn, sql, (row, value, u))

    async def _insert_plain(self, row: str, value: int, u: int) -> str:
        sql = (f"INSERT INTO {self.table} (k, v, u) VALUES "
               + ("(%s, %s, %s)" if self.d.name == "psycopg" else "($1, $2, $3)"))
        return await self.d.execute(self.conn, sql, (row, value, u))

    # -- the official CM, manually driven -----------------------------
    async def _cm_block(self, holder: dict, out: dict, body) -> dict:
        """enter / body / exit with the phase label set before each await.

        `body` is an async callable.  An error (including CancelledError) from
        the body drives `__aexit__` with that exception, then is re-raised; an
        error at exit is recorded and re-raised.  No exception is swallowed.
        """
        tx = self.conn.transaction()
        out.setdefault("stages", [])
        try:
            holder["phase"] = "enter"
            if self.d.name == "asyncpg":
                # Remember the object before awaiting: a cancelled entry must
                # still be able to name what _top_xact points at.
                self.txobj = tx
                await tx.__aenter__()
            else:
                # psycopg's CM yields its internal AsyncTransaction.
                self.txobj = await tx.__aenter__()
            out["stages"].append("enter")
        except BaseException as exc:
            out["failing_stage"] = "enter"
            out["exc"] = exc_info(exc)
            self._event("cm_enter_failed", exc=exc_info(exc),
                        client=self.d.client_state(self.conn, None))
            raise
        try:
            holder["phase"] = "body"
            out["body_result"] = await body()
            out["stages"].append("body")
        except BaseException as exc:
            out["failing_stage"] = "body"
            out["exc"] = exc_info(exc)
            self._event("cm_body_failed", exc=exc_info(exc),
                        client=self.d.client_state(self.conn, self.txobj))
            holder["phase"] = "exit_rollback"
            try:
                await tx.__aexit__(type(exc), exc, exc.__traceback__)
                out["aexit"] = "rollback"
                self._event("cm_rollback_ok",
                            client=self.d.client_state(self.conn, self.txobj))
            except BaseException as cleanup:
                out["aexit"] = "rollback"
                out["aexit_exc"] = exc_info(cleanup)
                self._event("cm_rollback_failed", exc=exc_info(cleanup),
                            client=self.d.client_state(self.conn, self.txobj))
                raise
            raise
        holder["phase"] = "exit_commit"
        try:
            await tx.__aexit__(None, None, None)
            out["aexit"] = "commit"
            out["stages"].append("commit")
            self._event("cm_commit_ok",
                        client=self.d.client_state(self.conn, self.txobj))
        except BaseException as exc:
            out["aexit"] = "commit"
            out["aexit_exc"] = exc_info(exc)
            self._event("cm_commit_failed", exc=exc_info(exc),
                        client=self.d.client_state(self.conn, self.txobj))
            raise
        return out

    # -- successor / recovery -----------------------------------------
    async def successor(self, row: str, value: int, label: str = "cm") -> dict:
        out: dict = {}
        self.txobj = None       # a failed successor entry has no reachable object
        holder: dict = {"phase": f"successor_{label}"}
        started = time.monotonic()
        rec = {"label": label, "ok": False, "timed_out": False}
        try:
            await asyncio.wait_for(
                self._cm_block(holder, out, lambda: self._insert(row, value, value)),
                TRIAL_TIMEOUT,
            )
            rec["ok"] = True
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            rec["exc"] = exc_info(exc)
            if isinstance(exc, asyncio.TimeoutError):
                rec["timed_out"] = True
        rec["stages"] = out.get("stages", [])
        rec["aexit"] = out.get("aexit")
        rec["aexit_exc"] = out.get("aexit_exc")
        rec["body_result"] = out.get("body_result")
        rec["client_after"] = self.d.client_state(self.conn, self.txobj)
        rec["seconds"] = round(time.monotonic() - started, 4)
        try:
            rec["server_after"] = await asyncio.wait_for(self._server_sample(), BOUND)
        except BaseException as exc:
            rec["server_after"] = {"error": err_text(exc)}
        try:
            rec["rows_after"] = await asyncio.wait_for(self._observer_rows(), BOUND)
        except BaseException as exc:
            rec["rows_after"] = {"error": err_text(exc)}
        self._event("successor", label=label, rec=rec)
        return rec

    async def recovery(self) -> dict:
        """Attempt the driver's public cleanup, then record the next dead end."""
        rec: dict = {}
        if self.d.name == "psycopg":
            r1 = await bound(self.conn.rollback())
            rec["rollback"] = list(r1)
            rec["client_after_rollback"] = self.d.client_state(self.conn, self.txobj)
            if r1[0] != "ok":
                r2 = await bound(self._execute_raw("ROLLBACK"))
                rec["raw_rollback"] = list(r2)
                rec["client_after_raw"] = self.d.client_state(self.conn, self.txobj)
                if r2[0] == "ok":
                    rec["retry_after_raw"] = await self.successor(
                        RETRY2_ROW, 6, label="after_raw_rollback"
                    )
        else:
            r1 = await bound(self.conn.reset())
            rec["reset"] = list(r1)
            rec["client_after_reset"] = self.d.client_state(self.conn, self.txobj)
        self._event("recovery", rec=rec)
        return rec

    async def _execute_raw(self, sql: str):
        return await self.d.execute(self.conn, sql)

    async def reconnect(self) -> dict:
        old = self.pid_value
        await close_quiet(self.conn.close())
        self.conn = must_ok(
            "reconnect",
            await bound(self.d.connect(self.proxy_port,
                                      autocommit=(self.d.name == "psycopg"))),
        )
        self.pid_value = await self.d.pid(self.conn)
        self.d.pid_value = self.pid_value
        self._event("reconnect", old_pid=old, new_pid=self.pid_value)
        return {"old_pid": old, "new_pid": self.pid_value}


# ---------------------------------------------------------------------------
# arms
# ---------------------------------------------------------------------------
async def arm_normal(sc: Scenario, holder, out) -> None:
    await sc._cm_block(holder, out, lambda: sc._insert(TARGET_ROW, 1, 1))


async def arm_server_body_div(sc: Scenario, holder, out) -> None:
    async def body():
        await sc._insert(TARGET_ROW, 1, 1)
        await sc.d.execute(sc.conn, "SELECT 1/0")
    await sc._cm_block(holder, out, body)


async def arm_server_body_unique(sc: Scenario, holder, out) -> None:
    async def body():
        await sc._insert_plain(TARGET_ROW, 1, 1)
        await sc._insert_plain(TARGET_ROW, 1, 1)
    await sc._cm_block(holder, out, body)


async def arm_server_body_swallowed(sc: Scenario, holder, out) -> None:
    async def body():
        await sc._insert(TARGET_ROW, 1, 1)
        try:
            await sc.d.execute(sc.conn, "SELECT 1/0")
        except BaseException as exc:
            out["swallowed_exc"] = exc_info(exc)
    await sc._cm_block(holder, out, body)


async def arm_server_exit_commit_deferred(sc: Scenario, holder, out) -> None:
    async def body():
        await sc.d.execute(
            sc.conn,
            f"INSERT INTO {sc.table} (k, v, u) VALUES ('d1', 1, 99), ('d2', 2, 99)",
        )
    await sc._cm_block(holder, out, body)


async def arm_cancel(sc: Scenario, k: int, double: bool = False):
    holder: dict = {"phase": "setup"}
    out: dict = {}
    coro = sc._cm_block(holder, out, lambda: sc._insert(TARGET_ROW, 1, 1))
    tr, meta = await _await_phased(
        coro, cut_after=k, timeout=CANCEL_TIMEOUT, holder=holder, double=double
    )
    await asyncio.sleep(sc.args.settle)
    return tr, meta, out


async def arm_server_entry_savepoint(sc: Scenario) -> dict:
    """A real 25P02 from SAVEPOINT entry inside an aborted transaction.

    Outer block is managed (both drivers), its body error is caught, then a
    nested transaction is entered: PostgreSQL refuses the SAVEPOINT because the
    transaction is already aborted.  This is the server-error counterpart of an
    entry that never yields a usable block.
    """
    rec: dict = {"phases": []}
    holder: dict = {"phase": "outer_enter"}
    outer = sc.conn.transaction()
    out_outer: dict = {}
    try:
        if sc.d.name == "asyncpg":
            sc.txobj = outer
            await outer.__aenter__()
        else:
            sc.txobj = await outer.__aenter__()
        rec["outer_enter"] = "ok"
    except BaseException as exc:
        rec["outer_enter"] = "failed"
        rec["outer_enter_exc"] = exc_info(exc)
        return rec
    await sc._insert(TARGET_ROW, 1, 1)
    try:
        await sc.d.execute(sc.conn, "SELECT 1/0")
    except BaseException as exc:
        rec["outer_body_exc"] = exc_info(exc)
    rec["client_after_outer_body_error"] = sc.d.client_state(sc.conn, sc.txobj)
    rec["server_after_outer_body_error"] = await sc._server_sample()
    nested = sc.conn.transaction()
    try:
        await nested.__aenter__()
        rec["nested_enter"] = "ok"
    except BaseException as exc:
        rec["nested_enter"] = "failed"
        rec["nested_enter_exc"] = exc_info(exc)
    try:
        await outer.__aexit__(None, None, None)
        rec["outer_exit"] = "ok"
    except BaseException as exc:
        rec["outer_exit"] = "failed"
        rec["outer_exit_exc"] = exc_info(exc)
    rec["client_after_outer_exit"] = sc.d.client_state(sc.conn, sc.txobj)
    rec["server_after_outer_exit"] = await sc._server_sample()
    rec["rows_before_successor"] = await sc._observer_rows()
    rec["successor"] = await sc.successor(RETRY_ROW, SUCCESSOR_VALUE,
                                          label="entry_savepoint")
    rec["recovery"] = await sc.recovery()
    rec["client_final"] = sc.d.client_state(sc.conn, sc.txobj)
    rec["server_final"] = await sc._server_sample()
    rec["rows_final"] = await sc._observer_rows()
    return rec


async def arm_server_entry_terminated(sc: Scenario) -> dict:
    """Real server-side session termination, then the outer CM entry."""
    rec: dict = {}
    pid = sc.pid_value
    terminated = await bound(
        sc.d.execute(sc.observer, "SELECT pg_terminate_backend("
                    + ("%s" if sc.d.name == "psycopg" else "$1") + ")", (pid,))
    )
    rec["terminate"] = list(terminated)
    await asyncio.sleep(sc.args.settle)
    rec["client_before_entry"] = sc.d.client_state(sc.conn, None)
    holder: dict = {"phase": "entry_terminated"}
    out: dict = {}
    try:
        await sc._cm_block(holder, out, lambda: sc._insert(TARGET_ROW, 1, 1))
        rec["entry"] = "ok"
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        rec["entry"] = "failed"
        rec["entry_exc"] = exc_info(exc)
        rec["entry_tb"] = traceback.format_exc().strip().splitlines()[-4:]
    rec["client_after_failure"] = sc.d.client_state(sc.conn, None)
    rec["server_after_failure"] = await sc._server_sample()
    rec["failure_recovery"] = await sc.recovery()
    try:
        rec["reconnect"] = await sc.reconnect()
        rec["successor"] = await sc.successor(RETRY_ROW, SUCCESSOR_VALUE,
                                              label="after_reconnect")
    except BaseException as exc:
        rec["reconnect_error"] = exc_info(exc)
    rec["rows_final"] = await sc._observer_rows()
    return rec


# ---------------------------------------------------------------------------
# one trial
# ---------------------------------------------------------------------------
PLAIN_ARMS = {
    "server_body_div": (arm_server_body_div, "22012"),
    "server_body_unique": (arm_server_body_unique, "23505"),
    "server_body_swallowed_commit": (arm_server_body_swallowed, "22012"),
    "server_exit_commit_deferred": (arm_server_exit_commit_deferred, "23505"),
}


async def _successor_and_recovery(sc: Scenario, rec: dict, arm: str) -> None:
    rec["successor"] = await sc.successor(RETRY_ROW, SUCCESSOR_VALUE, label=arm)
    rec["recovery"] = await sc.recovery()
    rec["client_final"] = sc.d.client_state(sc.conn, sc.txobj)
    rec["server_final"] = await sc._server_sample()
    rec["rows_final"] = await sc._observer_rows()


def _lift_detail(rec: dict, arm: str) -> None:
    detail = rec.get("detail") or {}
    rec["successor"] = detail.get("successor")
    rec["recovery"] = detail.get("recovery") or detail.get("failure_recovery")
    rec["rows_final"] = detail.get("rows_final")
    rec["rows_before_successor"] = detail.get("rows_before_successor")
    rec["client_final"] = detail.get("client_final")
    rec["server_final"] = detail.get("server_final")
    rec["client_after_target"] = (detail.get("client_after_outer_exit")
                                  or detail.get("client_after_failure"))
    rec["server_after_target"] = (detail.get("server_after_outer_exit")
                                  or detail.get("server_after_failure"))
    rec["responsibility_holder"] = HOLDERS[arm]


async def run_trial(args, driver: Driver, arm: str, *, k: int | None = None,
                    rep: int = 0, do_successor: bool = True) -> dict:
    sc = Scenario(driver, args)
    started = time.monotonic()
    rec: dict = {"trial_id": "", "driver": driver.name, "arm": arm,
                 "k": k, "rep": rep, "unrealized": None,
                 "responsibility_holder": None, "seconds": None}
    try:
        await asyncio.wait_for(sc.setup(), TRIAL_TIMEOUT)
        rec["trial_id"] = f"{driver.name}-{arm}-{rep}-{sc.table.rsplit('_', 1)[-1]}"
        rec["table"] = sc.table
        sc.txobj = None          # do not leak the preflight transaction object
        holder: dict = {"phase": "setup"}
        out: dict = {}
        if arm.startswith("cancel"):
            tr, meta, out = await arm_cancel(sc, k)
            realized = bool(tr.cut_requested and tr.outcome in ("cancelled", "error"))
            rec["cut"] = {"phase": meta["cut_phase"],
                          "second_phase": meta["second_phase"],
                          "requested": tr.cut_requested,
                          "outcome": tr.outcome,
                          "yields": tr.yields,
                          "seconds": round(tr.seconds, 3)}
            rec["realized"] = realized
            if not realized:
                rec["unrealized"] = (f"cut_requested={tr.cut_requested} "
                                     f"outcome={tr.outcome}")
            rec["target"] = {"outcome": tr.outcome, "exc": out.get("exc"),
                             "stages": out.get("stages", []),
                             "aexit": out.get("aexit"),
                             "aexit_exc": out.get("aexit_exc")}
            rec["client_after_target"] = driver.client_state(sc.conn, sc.txobj)
            rec["server_after_target"] = await sc._server_sample()
            rec["rows_after_target"] = await sc._observer_rows()
            rec["responsibility_holder"] = HOLDERS[arm]
            if do_successor and realized:
                await _successor_and_recovery(sc, rec, arm)
        elif arm == "normal":
            try:
                await sc._cm_block(holder, out,
                                   lambda: sc._insert(TARGET_ROW, 1, 1))
                rec["target"] = {"outcome": "completed", "stages": out.get("stages"),
                                 "aexit": out.get("aexit")}
            except BaseException as exc:
                rec["target"] = {"outcome": "error", "exc": exc_info(exc),
                                 "stages": out.get("stages"),
                                 "aexit": out.get("aexit")}
            rec["client_after_target"] = driver.client_state(sc.conn, sc.txobj)
            rec["server_after_target"] = await sc._server_sample()
            rec["rows_after_target"] = await sc._observer_rows()
            rec["responsibility_holder"] = HOLDERS["normal"]
            rec["realized"] = (rec["target"]["outcome"] == "completed"
                               and rec["rows_after_target"] == [[TARGET_ROW, 1]])
            if not rec["realized"]:
                rec["unrealized"] = "normal control did not commit its write"
            rec["recovery"] = await sc.recovery()
            rec["client_final"] = driver.client_state(sc.conn, sc.txobj)
            rec["server_final"] = await sc._server_sample()
            rec["rows_final"] = await sc._observer_rows()
        elif arm in PLAIN_ARMS:
            fn, expect = PLAIN_ARMS[arm]
            await _run_plain_arm(sc, rec, fn)
            rec["expected_sqlstate"] = expect
            got = ((rec.get("target") or {}).get("exc") or {}).get("sqlstate")
            if arm == "server_body_swallowed_commit":
                got = (rec.get("swallowed_exc") or {}).get("sqlstate")
                ok = (rec["target"]["outcome"] == "completed" and got == expect)
            else:
                ok = (rec["target"]["outcome"] == "error" and got == expect)
            rec["realized"] = bool(ok)
            if not rec["realized"]:
                rec["unrealized"] = (f"expected {expect}, got {got} "
                                     f"(target {rec['target']['outcome']})")
            await asyncio.sleep(args.settle)
            rec["client_after_target"] = driver.client_state(sc.conn, sc.txobj)
            rec["server_after_target"] = await sc._server_sample()
            rec["rows_after_target"] = await sc._observer_rows()
            rec["responsibility_holder"] = HOLDERS[arm]
            if do_successor and rec["realized"]:
                await _successor_and_recovery(sc, rec, arm)
        elif arm == "server_entry_savepoint":
            rec["detail"] = await arm_server_entry_savepoint(sc)
            _lift_detail(rec, arm)
            got = ((rec["detail"].get("nested_enter_exc") or {}).get("sqlstate"))
            rec["expected_sqlstate"] = "25P02"
            rec["realized"] = got == "25P02"
            if not rec["realized"]:
                rec["unrealized"] = f"expected 25P02, got {got}"
        elif arm == "server_entry_terminated":
            rec["detail"] = await arm_server_entry_terminated(sc)
            _lift_detail(rec, arm)
            rec["expected_sqlstate"] = "57P01 or closed connection"
            rec["realized"] = rec["detail"].get("entry") == "failed"
            if not rec["realized"]:
                rec["unrealized"] = "entry unexpectedly succeeded after termination"
        else:
            raise SystemExit(f"unknown arm {arm!r}")

        rec["events"] = sc.events
        rec["seconds"] = round(time.monotonic() - started, 3)
        return _judge(rec)
    except BaseException as exc:
        rec["harness_error"] = {"class": type(exc).__name__, "text": str(exc)[:400],
                                "tb": traceback.format_exc().strip().splitlines()[-5:]}
        rec["seconds"] = round(time.monotonic() - started, 3)
        if isinstance(exc, asyncio.CancelledError):
            raise
        return rec
    finally:
        with contextlib.suppress(BaseException):
            await sc.close()


async def _run_plain_arm(sc: Scenario, rec: dict, fn) -> None:
    holder: dict = {"phase": "setup"}
    out: dict = {}
    try:
        await fn(sc, holder, out)
        rec["target"] = {"outcome": "completed", "stages": out.get("stages"),
                         "aexit": out.get("aexit")}
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        rec["target"] = {"outcome": "error", "exc": exc_info(exc),
                         "stages": out.get("stages"),
                         "aexit": out.get("aexit"),
                         "aexit_exc": out.get("aexit_exc")}
    rec["swallowed_exc"] = out.get("swallowed_exc")


# ---------------------------------------------------------------------------
# judgement fields (descriptive, not a generic framework)
# ---------------------------------------------------------------------------
HOLDERS = {
    "normal": "CM __aexit__ -> COMMIT; no residual obligation",
    "cancel_enter": "no block exists; the driver-internal transaction object is left behind",
    "cancel_body": "CM __aexit__(CancelledError) -> ROLLBACK",
    "cancel_exit_commit": "driver wait() cancellation cleanup + ambiguous COMMIT",
    "server_body_div": "CM __aexit__(error) -> ROLLBACK",
    "server_body_unique": "CM __aexit__(error) -> ROLLBACK",
    "server_body_swallowed_commit": "caller swallowed the error; CM __aexit__(None) -> COMMIT of an aborted tx",
    "server_exit_commit_deferred": "server aborts the tx at COMMIT; driver reports 23505",
    "server_entry_savepoint": "no nested block; outer block and driver bookkeeping remain",
    "server_entry_terminated": "server closed the session; caller must reconnect",
}


def _judge(rec: dict) -> dict:
    """Derive the comparison fields once, from what was measured."""
    succ = rec.get("successor") or {}
    rows = rec.get("rows_final")
    rows_ok = isinstance(rows, list)
    retry_seen = rows_ok and any(r and r[0] == RETRY_ROW for r in rows)
    target_seen = rows_ok and any(r and r[0] == TARGET_ROW for r in rows)
    before = rec.get("rows_after_target")
    rec["retry_visible"] = retry_seen
    rec["target_visible"] = target_seen
    rec["target_visible_after_target"] = (
        isinstance(before, list) and any(r and r[0] == TARGET_ROW for r in before)
    )
    rec["target_silent_success"] = bool(
        (rec.get("target") or {}).get("outcome") == "completed" and not target_seen
    )
    rec["successor_ok"] = bool(succ.get("ok")) if succ else None
    rec["successor_silent_success"] = bool(succ.get("ok")) and not retry_seen
    before_successor = rec.get("rows_before_successor")
    target_before = (isinstance(before_successor, list)
                     and any(r and r[0] == TARGET_ROW for r in before_successor))
    rec["target_visible_before_successor"] = target_before
    rec["silent_exit_after_handled_error"] = bool(
        (rec.get("detail") or {}).get("outer_exit") == "ok" and not target_before
    )
    client = rec.get("client_after_target") or {}
    rec["client_idle"] = (
        client.get("status") == "IDLE" and client.get("num_transactions") == 0
    ) if client.get("kind") == "psycopg" else (
        client.get("in_transaction") is False
    )
    rec["connection_closed"] = bool(client.get("closed"))
    rec["cleanup_obligation"] = _cleanup_obligation(rec, client, succ)
    return rec


def _cleanup_obligation(rec: dict, client: dict, succ: dict) -> str:
    arm = rec["arm"]
    if rec.get("connection_closed"):
        return "reconnect (connection closed by server or driver)"
    if arm == "normal":
        return "none"
    if arm in ("server_body_div", "server_body_unique",
               "server_exit_commit_deferred"):
        return "none observed: successor CM succeeded and its write was visible"
    if arm == "server_body_swallowed_commit":
        return ("caller must verify the COMMIT result; the client reports "
                "success while the server rolled back")
    if arm == "server_entry_terminated":
        return "reconnect"
    if arm == "cancel_enter":
        if client.get("kind") == "asyncpg":
            return ("ownerless _top_xact remains; reset()/reconnect required, "
                    "a fresh CM silently nests")
        return ("driver-internal bookkeeping: rollback() rejected, raw ROLLBACK "
                "does not reset the CM; close/reconnect required")
    if arm == "server_entry_savepoint":
        if client.get("kind") == "psycopg":
            return ("driver-internal bookkeeping after a failed __aenter__: "
                    "rollback()/raw ROLLBACK do not restore the CM")
        return "none observed: the live outer block's exit cleared the state"
    if arm == "cancel_body":
        return "none observed: rollback ran and the successor was visible"
    if arm == "cancel_exit_commit":
        return ("none for reuse, but the cancelled COMMIT's outcome is not "
                "resolvable by the client")
    return "see measured fields"


# ---------------------------------------------------------------------------
# driver-level phase calibration
# ---------------------------------------------------------------------------
async def calibrate(args, driver: Driver, outdir: str) -> dict:
    path = os.path.join(outdir, "calibration.jsonl")
    # A non-cancelling _PhasedTask with cut_after=None gives the yield count.
    yields = 0
    sc = Scenario(driver, args)
    try:
        await asyncio.wait_for(sc.setup(), TRIAL_TIMEOUT)
        sc.txobj = None
        holder = {"phase": "count"}
        tr = await asyncio.wait_for(
            _await_target_plain(sc, holder, None), TRIAL_TIMEOUT
        )
        yields = tr.yields
        count_rec = {"driver": driver.name, "kind": "count",
                     "outcome": tr.outcome, "yields": yields,
                     "seconds": round(tr.seconds, 3)}
        print("CALIB " + json.dumps(count_rec), flush=True)
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(count_rec, sort_keys=True) + "\n")
    finally:
        await sc.close()
    if yields == 0:
        raise RuntimeError(f"{driver.name}: target had no suspension")

    phases: dict[str, list[int]] = {}
    for k in range(1, yields + 1):
        sc = Scenario(driver, args)
        try:
            await asyncio.wait_for(sc.setup(), TRIAL_TIMEOUT)
            sc.txobj = None
            tr, meta, out = await arm_cancel(sc, k)
            rows = await sc._observer_rows()
            rec = {
                "driver": driver.name, "kind": "phase_map", "k": k,
                "cut_phase": meta["cut_phase"],
                "outcome": tr.outcome, "realized": bool(tr.realized),
                "aexit": out.get("aexit"),
                "aexit_exc": out.get("aexit_exc"),
                "exc": out.get("exc"),
                "client_after": driver.client_state(sc.conn, sc.txobj),
                "observer_rows": rows,
                "seconds": round(tr.seconds, 3),
            }
            print("CALIB " + json.dumps(sanitize(rec, sc.table), sort_keys=True),
                  flush=True)
            with open(path, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(sanitize(rec, sc.table), sort_keys=True) + "\n")
            if tr.realized and meta["cut_phase"]:
                phases.setdefault(meta["cut_phase"], []).append(k)
        finally:
            await sc.close()
    result = {"driver": driver.name, "yields": yields,
              "phases": {p: ks for p, ks in phases.items() if ks}}
    print("CALIB-PHASES " + json.dumps(result, sort_keys=True), flush=True)
    with open(os.path.join(outdir, f"calibration-{driver.name}.json"), "w",
              encoding="utf-8") as fh:
        json.dump(result, fh, indent=2, sort_keys=True)
    return result


async def _await_target_plain(sc: Scenario, holder: dict, k):
    from dropwise_py.engine import _await_target
    out: dict = {}
    coro = sc._cm_block(holder, out, lambda: sc._insert(TARGET_ROW, 1, 1))
    return await _await_target(coro, k, TRIAL_TIMEOUT)


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
async def run_driver(args, driver: Driver, outdir: str) -> dict:
    trial_path = os.path.join(outdir, "trials.jsonl")
    calib = await calibrate(args, driver, outdir)
    if args.calibrate_only:
        return {"driver": driver.name, "counts": {}, "totals": {},
                "calibration": calib}
    reps = args.reps
    phase_ks = {p: ks[0] for p, ks in calib["phases"].items()
                if p in ("enter", "body", "exit_commit")}
    if len(phase_ks) < 3:
        print(f"WARNING {driver.name}: only phases {sorted(calib['phases'])}; "
              "cancellation comparison is partial", flush=True)
    counts: dict[str, int] = {}
    totals: dict[str, int] = {}

    def emit(rec: dict) -> None:
        rec = sanitize(rec, rec.get("table"))
        with open(trial_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, default=str, sort_keys=True) + "\n")
        arm = rec.get("arm", "?")
        counts[arm] = counts.get(arm, 0) + 1
        totals["trials"] = totals.get("trials", 0) + 1
        print("TRIAL " + json.dumps({
            "id": rec.get("trial_id"), "driver": rec.get("driver"),
            "arm": arm, "k": rec.get("k"), "rep": rec.get("rep"),
            "realized": rec.get("realized"),
            "cut": (rec.get("cut") or {}).get("phase"),
            "exc": ((rec.get("target") or {}).get("exc") or {}).get("class"),
            "sqlstate": ((rec.get("target") or {}).get("exc") or {}).get("sqlstate"),
            "client": rec.get("client_after_target"),
            "server": (rec.get("server_after_target") or {}).get("state"),
            "successor_ok": rec.get("successor_ok"),
            "retry_visible": rec.get("retry_visible"),
            "silent": rec.get("successor_silent_success"),
            "rows": rec.get("rows_final"),
            "cleanup": rec.get("cleanup_obligation"),
            "seconds": rec.get("seconds"),
        }, default=str, sort_keys=True), flush=True)

    # normal control
    for rep in range(1, reps + 1):
        emit(await run_trial(args, driver, "normal", rep=rep))
    # cancellation by semantic phase
    for phase, k in phase_ks.items():
        arm = f"cancel_{phase}"
        for rep in range(1, reps + 1):
            emit(await run_trial(args, driver, arm, k=k, rep=rep))
    # ordinary server failures
    for arm in ("server_body_div", "server_body_unique",
                "server_body_swallowed_commit", "server_exit_commit_deferred",
                "server_entry_savepoint", "server_entry_terminated"):
        for rep in range(1, reps + 1):
            emit(await run_trial(args, driver, arm, rep=rep))
    return {"driver": driver.name, "counts": counts, "totals": totals,
            "calibration": calib}


async def amain(args) -> None:
    os.makedirs(args.outdir, exist_ok=True)
    meta = {
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "cwd": os.getcwd(),
        "python": sys.version,
        "psycopg": importlib.metadata.version("psycopg"),
        "asyncpg": importlib.metadata.version("asyncpg"),
        "server": f"{args.host}:{args.port}/{args.dbname}",
        "delay": args.delay, "settle": args.settle,
        "reps": args.reps, "timeout": TRIAL_TIMEOUT,
        "drivers": args.driver, "outdir": args.outdir,
    }
    print("META " + json.dumps(meta, sort_keys=True), flush=True)
    drivers = ["psycopg", "asyncpg"] if args.driver == "both" else [args.driver]
    results = []
    for name in drivers:
        d = Driver(name, args)
        results.append(await run_driver(args, d, args.outdir))
    meta["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    meta["results"] = results
    with open(os.path.join(args.outdir, "meta.json"), "w", encoding="utf-8") as fh:
        json.dump(meta, fh, indent=2, sort_keys=True)
    print("DONE " + json.dumps(results, sort_keys=True), flush=True)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=32870)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--driver", default="both",
                   choices=["psycopg", "asyncpg", "both"])
    p.add_argument("--reps", type=int, default=3)
    p.add_argument("--delay", type=float, default=DELAY)
    p.add_argument("--settle", type=float, default=SETTLE)
    p.add_argument("--outdir", default=None)
    p.add_argument("--calibrate-only", action="store_true")
    args = p.parse_args()
    if args.outdir is None:
        args.outdir = os.path.join("failure_compare", "logs",
                                   "run.fc." + time.strftime("%Y%m%d-%H%M%S"))
    asyncio.run(amain(args))


if __name__ == "__main__":
    main()
