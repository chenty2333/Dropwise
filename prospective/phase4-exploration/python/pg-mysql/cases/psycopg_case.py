"""psycopg 3.3.6 + psycopg-pool 3.3.3 cancellation cases.

Two reuse units, same skeleton:

  pool    AsyncConnectionPool(min_size=1, max_size=1), autocommit through
          `configure`; target checks out a connection with `getconn()`, runs
          BEGIN / upsert(target) / COMMIT, and returns it with `putconn()`.
  direct  one long-lived AsyncConnection (autocommit=True); the target only
          borrows it, the survivor uses the same object.

Target = explicit transaction that writes the target row and commits
(BEGIN + write + COMMIT are three awaits in the same target coroutine).
Survivor = same pool/handle, upserts the survivor row in the same table and
reads it back on the same connection.  Observer = separate direct connection,
not through the pool and not through the delay proxy.

Pending mechanism for both modes: `DelayProxy` (retro `cases/common.py`,
50 ms on every server->client chunk) wrapping only the tested connection.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import time

import psycopg
import psycopg_pool

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
from dropwise_py.engine import _await_target  # noqa: E402

TARGET_ROW = "target"
SURVIVOR_ROW = "survivor"
B3_ROW = "b3"

TX_NAMES = {0: "IDLE", 1: "ACTIVE", 2: "INTRANS", 3: "INERROR", 4: "UNKNOWN"}


def tx_name(value) -> str:
    try:
        return TX_NAMES.get(int(value), str(value))
    except Exception:
        return str(value)


def conninfo(host: str, port: int, user: str, password: str, dbname: str) -> str:
    return (
        f"host={host} port={port} user={user} password={password} "
        f"dbname={dbname} connect_timeout=5"
    )


class PsycopgScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.table = unique_table("dw_r3_pg")
        self.proxy: DelayProxy | None = None
        self.pool: psycopg_pool.AsyncConnectionPool | None = None
        self.conn: psycopg.AsyncConnection | None = None
        self.observer: psycopg.AsyncConnection | None = None
        # diagnostics only (never part of the judged observation)
        self.diag: dict = {}

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        direct = conninfo(a.host, a.port, a.user, a.password, a.dbname)
        via_proxy = conninfo(a.host, client_port, a.user, a.password, a.dbname)

        # observer first, direct to the server (no proxy, no tested pool)
        obs = await bound(psycopg.AsyncConnection.connect(direct, autocommit=True))
        if obs[0] != "ok":
            raise RuntimeError(f"observer connect failed: {obs}")
        self.observer = obs[1]
        created = await bound(
            self.observer.execute(
                f"CREATE TABLE {self.table} (k text PRIMARY KEY, v integer NOT NULL)"
            )
        )
        if created[0] != "ok":
            raise RuntimeError(f"create table failed: {created}")

        if self.mode == "pool":
            async def configure(conn):
                await conn.set_autocommit(True)

            self.pool = psycopg_pool.AsyncConnectionPool(
                via_proxy,
                min_size=1,
                max_size=1,
                open=False,
                configure=configure,
                timeout=BOUND,
            )
            opened = await bound(self.pool.open(wait=True, timeout=BOUND))
            if opened[0] != "ok":
                raise RuntimeError(f"pool open failed: {opened}")
            # warm-up: consume the handshake on the tested connection
            warm = await bound(self._pool_exec("SELECT 1"))
            if warm[0] != "ok":
                raise RuntimeError(f"warm-up failed: {warm}")
            self.diag["setup_pid"] = await self._pid_pool()
        else:
            self.conn = await bound(
                psycopg.AsyncConnection.connect(via_proxy, autocommit=True)
            )
            if self.conn[0] != "ok":
                raise RuntimeError(f"tested connect failed: {self.conn}")
            self.conn = self.conn[1]
            warm = await bound(self._exec(self.conn, "SELECT 1"))
            if warm[0] != "ok":
                raise RuntimeError(f"warm-up failed: {warm}")
            self.diag["setup_pid"] = self.conn.info.backend_pid
        self.diag["setup_stats"] = self._stats()

    async def _pool_exec(self, sql: str, params=None):
        conn = await self.pool.getconn(timeout=BOUND)
        try:
            return await self._exec(conn, sql, params)
        finally:
            await self.pool.putconn(conn)

    async def _exec(self, conn, sql: str, params=None):
        cur = await conn.execute(sql, params)
        return cur.statusmessage

    async def _pid_pool(self) -> int | None:
        try:
            conn = await self.pool.getconn(timeout=BOUND)
        except BaseException:
            return None
        try:
            return conn.info.backend_pid
        finally:
            await self.pool.putconn(conn)

    def _stats(self) -> dict | None:
        if self.pool is None:
            return None
        try:
            return dict(self.pool.get_stats())
        except BaseException as exc:
            return {"error": type(exc).__name__}

    async def _acquire(self):
        if self.mode == "pool":
            return await self.pool.getconn(timeout=BOUND)
        return self.conn

    async def _release(self, conn) -> None:
        if self.mode == "pool" and conn is not None:
            await self.pool.putconn(conn)

    # ---- target ------------------------------------------------------
    async def _tx_write(self, row: str, value: int):
        """BEGIN / upsert(row, value) / COMMIT, on one checked-out conn."""
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return {"acquire": acq}
        conn = acq[1]
        out: dict = {
            "backend_pid": conn.info.backend_pid,
            "status_after_acquire": tx_name(conn.info.transaction_status),
        }
        stages = self.diag.setdefault("stages", [])
        try:
            out["begin"] = await bound(self._exec(conn, "BEGIN"))
            if out["begin"][0] == "ok":
                stages.append("begin")
            out["insert"] = await bound(
                self._exec(
                    conn,
                    f"INSERT INTO {self.table} (k, v) VALUES (%s, %s) "
                    "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
                    (row, value),
                )
            )
            if out["insert"][0] == "ok":
                stages.append("insert")
            out["commit"] = await bound(self._exec(conn, "COMMIT"))
            if out["commit"][0] == "ok":
                stages.append("commit")
        except BaseException as exc:
            out["escaped"] = f"{type(exc).__name__}: {exc}"
            raise
        finally:
            # state handed to the next user, recorded before any reset
            self.diag["after_target"] = {
                "pid": conn.info.backend_pid,
                "status": tx_name(conn.info.transaction_status),
                "pool_before_release": self._stats(),
            }
            out["status_before_release"] = tx_name(conn.info.transaction_status)
            rel = await bound(self._release(conn))
            out["release"] = rel
            self.diag["after_release"] = {
                "status": tx_name(conn.info.transaction_status),
                "pool": self._stats(),
            }
        return out

    async def target(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def same(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def different(self):
        return await self._tx_write(B3_ROW, 3)

    # ---- survivor / observer ----------------------------------------
    async def survivor(self):
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return {"acquire": acq, "in_tx": None, "write": None, "own": None, "release": None}
        conn = acq[1]
        acquire_status = tx_name(conn.info.transaction_status)
        self.diag["survivor_start"] = {
            "pid": conn.info.backend_pid,
            "status": acquire_status,
            "pool": self._stats(),
        }
        write = await bound(
            self._exec(
                conn,
                f"INSERT INTO {self.table} (k, v) VALUES (%s, %s) "
                "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
                (SURVIVOR_ROW, 7),
            )
        )
        own = None
        if write[0] != "timeout":
            own = await bound(self._read_scalar(conn, SURVIVOR_ROW))
        release = await bound(self._release(conn))
        self.diag["survivor_end"] = {
            "status": tx_name(conn.info.transaction_status),
            "pool": self._stats(),
        }
        return {"in_tx": acquire_status, "write": write, "own": own, "release": release}

    async def _read_scalar(self, conn, row: str):
        cur = await conn.execute(
            f"SELECT v FROM {self.table} WHERE k = %s", (row,)
        )
        got = await cur.fetchone()
        return None if got is None else got[0]

    async def observe(self):
        cur = await self.observer.execute(
            f"SELECT k, v FROM {self.table} ORDER BY k"
        )
        rows = await cur.fetchall()
        return {"rows": [[k, v] for k, v in rows], "error": None}

    # ---- cleanup -----------------------------------------------------
    async def close(self) -> None:
        if self.pool is not None:
            await close_quiet(self.pool.close(timeout=1.0))
        if self.conn is not None:
            await close_quiet(self.conn.close())
        if self.observer is not None:
            await close_quiet(self.observer.close())
        if self.proxy is not None:
            await close_quiet(self.proxy.close())

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.table)


# ---------------------------------------------------------------------
# diagnostics: reuse, pre/post, per-yield detail
# ---------------------------------------------------------------------
async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        pids = []
        for _ in range(3):
            v = await bound(sc._pid_pool() if sc.mode == "pool" else _pid(sc))
            pids.append(v)
        print(f"DIAG reuse pids={pids} stats={sc._stats()}", flush=True)

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


async def _pid(sc):
    return sc.conn.info.backend_pid


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


# ---------------------------------------------------------------------
def build_factory(args):
    return lambda: PsycopgScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument(
        "--diag", action="store_true", help="reuse + pre/post calibration details"
    )
    args = parser.parse_args()

    print(
        f"case=psycopg mode={args.mode} psycopg={psycopg.__version__} "
        f"psycopg_pool={psycopg_pool.__version__} python={__import__('sys').version.split()[0]} "
        f"host={args.host} port={args.port} db={args.dbname} round={args.round} "
        f"delay={DELAY} bound={BOUND}",
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
            r = await level_b1("psycopg", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "psycopg", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "psycopg", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed(
                "psycopg", factory, reps=args.reps, timeout=timeout
            )
        else:
            raise SystemExit(f"unknown level {level}")
        print_report(r)
        print(compact(r), flush=True)
        reports.append(r)

    print("=== FINAL ===", flush=True)
    for r in reports:
        print(f"mode={args.mode} {compact(r)}", flush=True)
        print(f"violations_by_plan={[t.plan for t in r.violations()]}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
