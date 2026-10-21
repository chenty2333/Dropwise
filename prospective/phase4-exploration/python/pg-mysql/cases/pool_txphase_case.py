"""Round-4 layer probe: what do psycopg_pool / asyncpg.Pool do with a
connection whose transaction *entry* was cancelled?

This is deliberately small and only uses the pooled public API:

    async with pool.connection() / pool.acquire() as conn:
        async with conn.transaction():      # cancelled in __aenter__
            ...

Four caller behaviours are compared:

    baseline    no cancellation, normal write
    propagate   cancellation escapes the pool block
    swallow     cancellation is swallowed, block exits normally
    retry_same  cancellation is caught and the transaction is retried on the
                same still-checked-out connection, then the cancellation escapes

The independent observer is a direct asyncpg connection, never behind the
delay proxy and never using the tested driver.  No driver or pool source is
modified.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import sys
import time

import asyncpg
import psycopg
import psycopg_pool

from cases.common import DelayProxy, bound, sanitize, unique_table
from cases.txphase_case import _await_phased

BOUND = 5.0


def err_text(exc: BaseException) -> str:
    return f"{type(exc).__name__}: {exc}"


class Capture(logging.Handler):
    def __init__(self) -> None:
        super().__init__(level=logging.WARNING)
        self.records: list[str] = []

    def emit(self, record: logging.LogRecord) -> None:
        try:
            self.records.append(record.getMessage())
        except BaseException:
            pass


class UnrealizedEntry(RuntimeError):
    """A probe failed to exercise the entry-cancellation contract."""


class PoolTx:
    def __init__(self, driver: str, host: str, port: int, proxy_port: int) -> None:
        self.driver = driver
        self.host = host
        self.port = port
        self.proxy_port = proxy_port
        self.table = unique_table("dw_r4pool")
        self.observer: asyncpg.Connection | None = None
        self.pool = None
        self.capture = Capture()
        self.old_pid: int | None = None
        self.results: list[dict] = []

    # ---- connections -------------------------------------------------
    def _psycopg_conninfo(self) -> str:
        return (f"host={self.host} port={self.proxy_port} user=postgres "
                f"password=dw dbname=dw")

    def _asyncpg_dsn(self) -> str:
        return f"postgresql://postgres:dw@{self.host}:{self.proxy_port}/dw"

    def _observer_dsn(self) -> str:
        return f"postgresql://postgres:dw@{self.host}:{self.port}/dw"

    async def setup(self) -> None:
        self.observer = await asyncpg.connect(self._observer_dsn())
        await self.observer.execute(
            f"CREATE TABLE {self.table} (name text primary key, value int)")
        logging.getLogger("psycopg").addHandler(self.capture)
        logging.getLogger("psycopg_pool").addHandler(self.capture)
        logging.getLogger("asyncpg").addHandler(self.capture)
        for name in ("psycopg", "psycopg_pool", "asyncpg"):
            logging.getLogger(name).setLevel(logging.WARNING)
        if self.driver == "psycopg":
            self.pool = psycopg_pool.AsyncConnectionPool(
                self._psycopg_conninfo(), min_size=1, max_size=1, open=False,
                timeout=BOUND)
            await self.pool.open()
            await self.pool.wait(timeout=BOUND)
        else:
            self.pool = await asyncpg.create_pool(
                self._asyncpg_dsn(), min_size=1, max_size=1,
                timeout=BOUND, command_timeout=BOUND)

    async def close(self) -> None:
        if self.pool is not None:
            await self.pool.close()
        if self.observer is not None:
            await self.observer.close()

    # ---- driver operations -------------------------------------------
    async def _pid(self, conn) -> int:
        if self.driver == "psycopg":
            return conn.info.backend_pid
        return conn.get_server_pid()

    async def _cm_write(self, conn, name: str, value: int) -> None:
        if self.driver == "psycopg":
            async with conn.transaction():
                await conn.execute(
                    f"INSERT INTO {self.table}(name, value) VALUES (%s, %s)",
                    (name, value))
        else:
            async with conn.transaction():
                await conn.execute(
                    f"INSERT INTO {self.table}(name, value) VALUES ($1, $2)",
                    name, value)

    async def _entry(self, conn, holder: dict):
        """Coroutine whose first suspension is inside the transaction entry."""
        holder["phase"] = "enter"
        async with conn.transaction():
            holder["phase"] = "body"
            await asyncio.sleep(0.5)

    async def _rows(self):
        recs = await self.observer.fetch(
            f"SELECT name, value FROM {self.table} ORDER BY name")
        return [[r["name"], r["value"]] for r in recs]

    async def _state(self, pid: int) -> dict:
        rec = await self.observer.fetchrow(
            "SELECT state, query FROM pg_stat_activity WHERE pid=$1", pid)
        if rec is None:
            return {"present": False}
        return {"present": True, "state": rec["state"], "query": rec["query"]}

    def _stats(self) -> dict:
        if self.driver == "psycopg":
            s = self.pool.get_stats()
            keys = ("pool_min", "pool_max", "pool_size", "pool_available",
                    "requests_waiting", "connections_num")
            return {k: s.get(k) for k in keys}
        return {"size": self.pool.get_size(), "idle": self.pool.get_idle_size(),
                "min": self.pool.get_min_size(), "max": self.pool.get_max_size()}

    async def _acquire_block(self, kind: str, holder: dict, out: dict) -> None:
        if self.driver == "psycopg":
            ctx = self.pool.connection()
        else:
            ctx = self.pool.acquire()
        async with ctx as conn:
            out["pid"] = await self._pid(conn)
            self.old_pid = out["pid"]
            out["server_before"] = await self._state(out["pid"])
            if kind == "baseline":
                out["target"] = await bound(self._cm_write(conn, "target", 1))
                return
            tr, meta = await _await_phased(
                self._entry(conn, holder), cut_after=1, timeout=5.0,
                holder=holder, double=False)
            out["target"] = {
                "outcome": tr.outcome, "realized": tr.realized,
                "yields": tr.yields,
            }
            out["cut_phase"] = meta["cut_phase"]
            if not tr.realized or tr.outcome != "cancelled" or meta["cut_phase"] != "enter":
                raise UnrealizedEntry(f"entry cancellation not realized: {out['target']}, {meta}")
            out["server_after_cut"] = await self._state(out["pid"])
            if kind == "retry_same":
                out["retry_same"] = list(await bound(self._cm_write(conn, "retry", 5)))
                out["rows_after_retry"] = await self._rows()
                out["server_after_retry"] = await self._state(out["pid"])
            if kind in ("propagate", "retry_same"):
                # Simulate the caller's task keeping the cancellation: the
                # cancellation itself is consumed by the engine above.
                raise asyncio.CancelledError

    async def one(self, kind: str, rep: int) -> dict:
        holder = {"phase": "setup"}
        warn0 = len(self.capture.records)
        out: dict = {"driver": self.driver, "kind": kind, "rep": rep,
                     "table": self.table}
        await self.observer.execute(f"TRUNCATE {self.table}")
        exit_kind = "none"
        try:
            await self._acquire_block(kind, holder, out)
        except asyncio.CancelledError:
            exit_kind = "cancelled"
        except UnrealizedEntry:
            raise
        except BaseException as exc:
            exit_kind = "error"
            out["exit_error"] = err_text(exc)
        out["exit"] = exit_kind
        out["warnings"] = [sanitize(w, self.table)
                           for w in self.capture.records[warn0:]]
        if self.old_pid is not None:
            out["old_pid_state_after_exit"] = await self._state(self.old_pid)
        out["stats_after_exit"] = self._stats()

        # A fresh checkout: did the pool hand out a clean connection?
        re: dict = {}
        try:
            async with (self.pool.connection() if self.driver == "psycopg"
                        else self.pool.acquire()) as conn:
                re["pid"] = await self._pid(conn)
                re["same_pid"] = re["pid"] == self.old_pid
                re["server"] = await self._state(re["pid"])
                re["post"] = list(await bound(self._cm_write(conn, "post", 9)))
                re["rows"] = await self._rows()
        except BaseException as exc:
            re["error"] = err_text(exc)
        out["reacquire"] = re
        out["rows_final"] = await self._rows()
        return out


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True,
                    help="direct PostgreSQL port (observer)")
    ap.add_argument("--driver", choices=["psycopg", "asyncpg"], required=True)
    ap.add_argument("--kinds", default="baseline,propagate,swallow,retry_same")
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--delay", type=float, default=0.05)
    args = ap.parse_args()

    proxy = DelayProxy(args.host, args.port, delay=args.delay)
    proxy_port = await proxy.start()
    sc = PoolTx(args.driver, args.host, args.port, proxy_port)
    await sc.setup()
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for kind in args.kinds.split(","):
                try:
                    out = await sc.one(kind, rep)
                except BaseException as exc:
                    failures += 1
                    out = {"driver": args.driver, "kind": kind, "rep": rep,
                           "trial_error": err_text(exc)}
                out = sanitize(out, sc.table)
                print("POOLTX", json.dumps(out, sort_keys=True), flush=True)
                summary = {
                    "driver": args.driver, "kind": kind, "rep": rep,
                    "exit": out.get("exit", "trial-error"),
                    "exit_error": out.get("exit_error"),
                    "old_pid_state_after_exit":
                        out.get("old_pid_state_after_exit"),
                    "target": out.get("target"),
                    "retry_same": out.get("retry_same"),
                    "rows_after_retry": out.get("rows_after_retry"),
                    "warnings": out.get("warnings"),
                    "reacquire": {k: out.get("reacquire", {}).get(k)
                                  for k in ("same_pid", "server", "rows",
                                            "error")},
                }
                print("POOLTX-SUMMARY", json.dumps(summary, sort_keys=True),
                      flush=True)
    finally:
        await sc.close()
        await proxy.close()
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
