"""mysql-connector-python 26.7.0 async direct-handle cancellation cases.

One long-lived `mysql.connector.aio.connect(autocommit=True)` handle created in
setup and reused by target/retry/survivor; no reconnect between the cancelled
target and the survivor.  Target = `start_transaction()` / `REPLACE(target)` /
`commit()` (three awaits).  Survivor = `REPLACE(survivor)` on the same handle +
same-connection readback, with the client-reported transaction state recorded
at the moment the survivor starts.  Observer = separate direct connection (no
proxy, not the tested handle) doing `SELECT k,v ORDER BY k`.

Pending mechanism: `DelayProxy` (retro `cases/common.py`, 50 ms per
server->client chunk) wrapping only the tested connection.  No server-side
SLEEP, no fixed settle.

`same()`/`different()` return a structured `OpResult`; a failed stage is
`ok=False`, is not followed by the next statement (so no COMMIT after a failed
insert), and B2/B3 use the engine's `operation_failed()` contract.  A broken
handle therefore becomes a fail-fast result, not a silent pass.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import time

import mysql.connector
import mysql.connector.aio as aio

from .common import (  # noqa: E402  (also puts research/python-retro on sys.path)
    BOUND,
    DELAY,
    DelayProxy,
    add_db_args,
    bound,
    close_quiet,
    compact,
    observation,
    unique_table,
)
from dropwise_py import (  # noqa: E402
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)
from dropwise_py.engine import OpResult, _await_target  # noqa: E402

TARGET_ROW = "target"
SURVIVOR_ROW = "survivor"
B3_ROW = "b3"


class MySQLConnectorScenario(Scenario):
    def __init__(self, args) -> None:
        if args.mode != "direct":
            raise SystemExit("mysqlconnector_case only supports --mode direct")
        self.args = args
        self.table = unique_table("dw_r3_mc")
        self.proxy: DelayProxy | None = None
        self.conn = None
        self.observer = None
        self.diag: dict = {}

    # ---- small helpers ----------------------------------------------
    def _state(self, conn) -> dict:
        """Client-reported state only; never part of a judged value's identity."""
        try:
            in_tx = bool(conn.in_transaction)
        except BaseException:
            in_tx = None
        try:
            unread = bool(conn.unread_result)
        except BaseException:
            unread = None
        return {
            "cid": getattr(conn, "connection_id", None),
            "in_tx": in_tx,
            "unread": unread,
        }

    async def _exec_conn(self, conn, sql: str, params=()):
        cur = await conn.cursor()
        try:
            await cur.execute(sql, params)
            # A statement with a result set must be consumed before the next
            # command, otherwise mysql-connector raises
            # "InternalError: Unread result found" (harness error, not a
            # cancellation effect).
            if cur.with_rows:
                await cur.fetchall()
            return cur.rowcount
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _fetch(self, conn, sql: str, params=()):
        cur = await conn.cursor()
        try:
            await cur.execute(sql, params)
            return await cur.fetchall()
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _connect(self, cfg: dict):
        return await aio.connect(**cfg, autocommit=True, connection_timeout=5)

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        common = dict(
            host=a.host,
            port=a.port,
            user=a.user,
            password=a.password,
            database=a.dbname,
        )
        proxy_cfg = dict(common, port=client_port)

        obs = await bound(self._connect(common))
        if obs[0] != "ok":
            raise RuntimeError(f"observer connect failed: {obs}")
        self.observer = obs[1]
        created = await bound(
            self._exec_conn(
                self.observer,
                f"CREATE TABLE {self.table} (k VARCHAR(32) PRIMARY KEY, v INT NOT NULL)",
            )
        )
        if created[0] != "ok":
            raise RuntimeError(f"create table failed: {created}")

        conn = await bound(self._connect(proxy_cfg))
        if conn[0] != "ok":
            raise RuntimeError(f"tested connect failed: {conn}")
        self.conn = conn[1]
        warm = await bound(self._exec_conn(self.conn, "SELECT 1"))
        if warm[0] != "ok":
            raise RuntimeError(f"warm-up failed: {warm}")
        self.diag["setup"] = self._state(self.conn)
        ac = await bound(self.conn.get_autocommit())
        connected = await bound(self.conn.is_connected())
        self.diag["setup_autocommit"] = ac
        self.diag["setup_connected"] = connected
        if ac[0] != "ok" or ac[1] is not True:
            raise RuntimeError(f"setup autocommit check failed: {ac}")
        if connected[0] != "ok" or connected[1] is not True:
            raise RuntimeError(f"setup connectivity check failed: {connected}")

    # ---- target ------------------------------------------------------
    async def _tx_write(self, row: str, value: int, *, retry: bool = False):
        """start_transaction / REPLACE(row) / commit, stop at first failure.

        `retry=True` (B2/B3) returns `OpResult(ok=False, stage=..., ...)` for a
        failed stage and never attempts the following statements.  The target
        (`retry=False`) raises, so an uncancelled stage failure cannot be
        calibrated as a legitimate post sample.
        """
        out = {
            "cid": getattr(self.conn, "connection_id", None),
            "in_tx_before": bool(self.conn.in_transaction),
            "start_transaction": None,
            "insert": None,
            "commit": None,
        }
        stages = self.diag.setdefault("stages", [])
        failed = None
        try:
            ops = (
                ("start_transaction", self.conn.start_transaction()),
                (
                    "insert",
                    self._exec_conn(
                        self.conn,
                        f"REPLACE INTO {self.table} (k, v) VALUES (%s, %s)",
                        (row, value),
                    ),
                ),
                ("commit", self.conn.commit()),
            )
            for name, coro in ops:
                res = await bound(coro)
                out[name] = res
                if res[0] != "ok":
                    failed = (name, res)
                    break
                stages.append(name)
        finally:
            self.diag["after_target"] = self._state(self.conn)
            out["in_tx_after"] = bool(self.conn.in_transaction)
        if failed is not None:
            name, res = failed
            if retry:
                return OpResult(False, name, value=out, error=f"{res[0]}: {res[1]}")
            raise RuntimeError(f"{name} failed: {res!r}")
        return OpResult(True, "commit", value=out)

    async def target(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def same(self):
        return await self._tx_write(TARGET_ROW, 1, retry=True)

    async def different(self):
        return await self._tx_write(B3_ROW, 3, retry=True)

    # ---- survivor / observer ----------------------------------------
    async def survivor(self):
        state = self._state(self.conn)
        self.diag["survivor_start"] = state
        write = await bound(
            self._exec_conn(
                self.conn,
                f"REPLACE INTO {self.table} (k, v) VALUES (%s, %s)",
                (SURVIVOR_ROW, 7),
            )
        )
        own = None
        if write[0] != "timeout":
            own = await bound(
                self._fetch(
                    self.conn,
                    f"SELECT v FROM {self.table} WHERE k = %s",
                    (SURVIVOR_ROW,),
                )
            )
            if own[0] == "ok":
                own = ("ok", [list(r) for r in own[1]])
        self.diag["survivor_end"] = self._state(self.conn)
        return {
            "in_tx": state["in_tx"],
            "write": write,
            "own": own,
            "release": None,  # direct handle: nothing to release
        }

    async def observe(self):
        rows = await self._fetch(
            self.observer, f"SELECT k, v FROM {self.table} ORDER BY k"
        )
        return {"rows": [list(r) for r in rows], "error": None}

    # ---- cleanup -----------------------------------------------------
    async def close(self) -> None:
        rec: dict = {}
        if self.conn is not None:
            rec["conn_close"] = await close_quiet(self.conn.close())
        if self.observer is not None:
            rec["observer_close"] = await close_quiet(self.observer.close())
        if self.proxy is not None:
            rec["proxy_close"] = await close_quiet(self.proxy.close())
        self.diag["close"] = rec
        bad = {k: v for k, v in rec.items() if v != "ok"}
        if bad:
            print(f"CLOSE_ERROR case=mysqlconnector {bad!r}", flush=True)

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.table)


# ---------------------------------------------------------------------
# diagnostics
# ---------------------------------------------------------------------
async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        print(f"DIAG setup={sc.diag!r}", flush=True)
        # reuse check: three round trips, connection id must not change
        ids = []
        for _ in range(3):
            st = await bound(sc._exec_conn(sc.conn, "SELECT CONNECTION_ID()"))
            r = await bound(sc._fetch(sc.conn, "SELECT CONNECTION_ID()"))
            ids.append((st, r))
        print(f"DIAG reuse={ids!r} state={sc._state(sc.conn)!r}", flush=True)

        pre = await sc.judged()
        print(f"DIAG pre obs={pre!r}", flush=True)
        tr = await _await_target(sc.target(), None, 12)
        print(
            f"DIAG post target={tr.outcome}/{tr.yields} result={tr.result!r} "
            f"diag={sc.diag!r}",
            flush=True,
        )
        post = await sc.judged()
        print(f"DIAG post obs={post!r}", flush=True)
        print(f"DIAG pre==post {pre == post}", flush=True)
    finally:
        await close_quiet(sc.close())


async def probe_run(factory, ks: list[int]) -> None:
    for k in ks:
        sc = factory()
        await asyncio.wait_for(sc.setup(), 12)
        started = time.monotonic()
        try:
            tr = await _await_target(sc.target(), k, 12)
            surv = await bound(sc.survivor(), 8)
            obs = await bound(sc.observe(), 8)
            print(
                f"PROBE k={k} target={tr.outcome} realized={tr.realized} "
                f"yields={tr.yields} err={tr.error!r} "
                f"diag={sc.diag!r}\n"
                f"        survivor={surv!r}\n"
                f"        observer={obs!r} "
                f"seconds={time.monotonic() - started:.2f}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


def build_factory(args):
    return lambda: MySQLConnectorScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument(
        "--diag", action="store_true", help="reuse + pre/post calibration details"
    )
    args = parser.parse_args()

    print(
        f"case=mysqlconnector mode={args.mode} "
        f"mysql-connector={mysql.connector.__version__} python="
        f"{__import__('sys').version.split()[0]} host={args.host} port={args.port} "
        f"db={args.dbname} round={args.round} delay={DELAY} bound={BOUND}",
        flush=True,
    )
    factory = build_factory(args)

    if args.probe:
        ks = [int(x) for x in args.probe.split(",") if x.strip()]
        await probe_run(factory, ks)
        return
    if args.diag:
        await diag_run(factory)
        return

    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    timeout = 12.0
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1("mysqlconnector", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "mysqlconnector", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "mysqlconnector", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed(
                "mysqlconnector", factory, reps=args.reps, timeout=timeout
            )
        else:
            raise SystemExit(f"unknown level {level}")
        print_report(r)
        print(compact(r), flush=True)
        reports.append(r)

    print("=== FINAL ===", flush=True)
    for r in reports:
        print(f"mode=direct {compact(r)}", flush=True)
        print(f"violations_by_plan={[t.plan for t in r.violations()]}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
