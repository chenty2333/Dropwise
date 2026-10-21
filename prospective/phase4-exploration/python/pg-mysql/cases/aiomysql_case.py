"""aiomysql 0.3.2 cancellation cases (direct handle; pool if time allowed).

Same skeleton as the PostgreSQL cases.  MariaDB 11.4, autocommit connection:
target = BEGIN / REPLACE(target) / COMMIT, survivor = REPLACE(survivor) +
same-connection readback, observer = independent direct aiomysql connection.
`Connection.get_transaction_status()` is the client-reported server status
(from the last OK packet); it is recorded at survivor acquire and judged
alongside the survivor's values.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import time

import aiomysql

from .common import (
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
from dropwise_py import (
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)
from dropwise_py.engine import OpResult, _await_target


class AiomysqlScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.table = unique_table("dw_r3_my")
        self.proxy: DelayProxy | None = None
        self.pool = None
        self.conn = None
        self.observer = None
        self.diag: dict = {}

    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        common = dict(
            host=a.host, port=a.port, user=a.user, password=a.password, db=a.dbname
        )
        proxy_cfg = dict(common, port=client_port)

        obs = await bound(aiomysql.connect(**common, autocommit=True))
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

        if self.mode == "pool":
            pool = await bound(
                aiomysql.create_pool(
                    minsize=1, maxsize=1, autocommit=True, **proxy_cfg
                )
            )
            if pool[0] != "ok":
                raise RuntimeError(f"pool create failed: {pool}")
            self.pool = pool[1]
        else:
            conn = await bound(aiomysql.connect(**proxy_cfg, autocommit=True))
            if conn[0] != "ok":
                raise RuntimeError(f"tested connect failed: {conn}")
            self.conn = conn[1]
        warm = await bound(self._acquire())
        if warm[0] != "ok":
            raise RuntimeError(f"warm-up acquire failed: {warm}")
        warm_exec = await bound(self._exec_conn(warm[1], "SELECT 1"))
        if warm_exec[0] != "ok":
            raise RuntimeError(f"warm-up query failed: {warm_exec}")
        warm_rel = await bound(self._release(warm[1]))
        if warm_rel[0] != "ok":
            raise RuntimeError(f"warm-up release failed: {warm_rel}")
        self.diag["setup"] = self._stats()

    def _stats(self):
        if self.pool is None:
            return None
        return {"size": self.pool.size, "free": self.pool.freesize, "max": self.pool.maxsize}

    async def _acquire(self):
        if self.mode == "pool":
            return await self.pool.acquire()
        return self.conn

    async def _release(self, conn) -> None:
        if self.mode == "pool" and conn is not None:
            await self.pool.release(conn)

    async def _exec_conn(self, conn, sql: str):
        cur = await conn.cursor()
        try:
            await cur.execute(sql)
            return cur.rowcount
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _fetch(self, conn, sql: str):
        cur = await conn.cursor()
        try:
            await cur.execute(sql)
            return await cur.fetchall()
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    def _txflag(self, conn):
        try:
            return int(conn.get_transaction_status())
        except BaseException as exc:
            return f"ERR {type(exc).__name__}"

    def _cid(self, conn):
        """Server-side connection id (diagnostic only, never judged)."""
        try:
            return int(conn.thread_id())
        except BaseException:
            try:
                return int(conn.thread_id)
            except BaseException:
                return None

    def _where(self, conn):
        return {
            "cid": self._cid(conn),
            "closed": bool(conn.closed),
            "txstatus": self._txflag(conn),
            "stats": self._stats(),
        }

    async def _tx_write(self, row: str, value: int, *, retry: bool = False):
        """BEGIN / REPLACE / COMMIT; stop at the first failed stage.

        `retry=True` (B2/B3) returns a structured `OpResult`: a failed stage is
        `ok=False` and COMMIT is never attempted after a failure.  The target
        (`retry=False`) raises instead so an uncancelled failure cannot become
        a calibratable "post" sample.
        """
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            if retry:
                return OpResult(False, "acquire", value=acq, error=f"{acq[0]}: {acq[1]}")
            raise RuntimeError(f"acquire failed: {acq!r}")
        conn = acq[1]
        out = {
            "cid": self._cid(conn),
            "in_tx": self._txflag(conn),
            "begin": None,
            "insert": None,
            "commit": None,
        }
        stages = self.diag.setdefault("stages", [])
        failed = None
        try:
            ops = (
                ("begin", self._exec_conn(conn, "BEGIN")),
                (
                    "insert",
                    self._exec_conn(
                        conn,
                        f"REPLACE INTO {self.table} (k,v) VALUES ('{row}', {value})",
                    ),
                ),
                ("commit", self._exec_conn(conn, "COMMIT")),
            )
            for name, coro in ops:
                res = await bound(coro)
                out[name] = res
                if res[0] != "ok":
                    failed = (name, res)
                    break
                stages.append(name)
        finally:
            self.diag["after_target"] = self._where(conn)
            rel = await bound(self._release(conn))
            out["release"] = rel
            self.diag["after_release"] = {
                "stats": self._stats(),
                "release": rel,
                "pool_free": getattr(self.pool, "freesize", None),
            }
        if failed is not None:
            name, res = failed
            if retry:
                return OpResult(False, name, value=out, error=f"{res[0]}: {res[1]}")
            raise RuntimeError(f"{name} failed: {res!r}")
        if out["release"][0] != "ok":
            if retry:
                return OpResult(
                    False, "release", value=out, error=f"release: {out['release']!r}"
                )
            raise RuntimeError(f"release failed: {out['release']!r}")
        return OpResult(True, "commit", value=out)

    async def target(self):
        return await self._tx_write("target", 1)

    async def same(self):
        return await self._tx_write("target", 1, retry=True)

    async def different(self):
        return await self._tx_write("b3", 3, retry=True)

    async def survivor(self):
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return {"acquire": acq, "in_tx": None, "write": None, "own": None, "release": None}
        conn = acq[1]
        in_tx = self._txflag(conn)
        self.diag["survivor_start"] = self._where(conn)
        write = await bound(
            self._exec_conn(conn, f"REPLACE INTO {self.table} (k,v) VALUES ('survivor', 7)")
        )
        own = None
        if write[0] != "timeout":
            own = await bound(
                self._fetch(conn, f"SELECT v FROM {self.table} WHERE k = 'survivor'")
            )
            if own[0] == "ok":
                own = ("ok", [list(r) for r in own[1]])
        release = await bound(self._release(conn))
        self.diag["survivor_end"] = {"stats": self._stats(), "release": release}
        return {"in_tx": in_tx, "write": write, "own": own, "release": release}

    async def observe(self):
        rows = await self._fetch(
            self.observer, f"SELECT k, v FROM {self.table} ORDER BY k"
        )
        return {"rows": [list(r) for r in rows], "error": None}

    async def close(self) -> None:
        rec: dict = {}
        if self.pool is not None:
            try:
                self.pool.close()
                rec["pool_close"] = "ok"
            except BaseException as exc:
                rec["pool_close"] = f"{type(exc).__name__}: {exc}"
            rec["pool_wait_closed"] = await close_quiet(self.pool.wait_closed())
        if self.conn is not None:
            # aiomysql Connection.close() is synchronous (only ensure_closed()
            # is a coroutine); awaiting it was a harness bug that recorded a
            # false TypeError in earlier close diagnostics.
            try:
                self.conn.close()
                rec["conn_close"] = "ok"
            except BaseException as exc:
                rec["conn_close"] = f"{type(exc).__name__}: {exc}"
        if self.observer is not None:
            try:
                self.observer.close()
                rec["observer_close"] = "ok"
            except BaseException as exc:
                rec["observer_close"] = f"{type(exc).__name__}: {exc}"
        if self.proxy is not None:
            rec["proxy_close"] = await close_quiet(self.proxy.close())
        self.diag["close"] = rec
        bad = {k: v for k, v in rec.items() if v != "ok"}
        if bad:
            print(f"CLOSE_ERROR case=aiomysql mode={self.mode} {bad!r}", flush=True)

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.table)


async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        print(f"DIAG setup={sc.diag!r}", flush=True)
        pre = await sc.judged()
        print(f"DIAG pre obs={pre!r}", flush=True)
        tr = await _await_target(sc.target(), None, 12)
        print(f"DIAG post target={tr.outcome}/{tr.yields} diag={sc.diag!r}", flush=True)
        post = await sc.judged()
        print(f"DIAG post obs={post!r}", flush=True)
        print(f"DIAG pre==post {pre == post}", flush=True)
    finally:
        await close_quiet(sc.close())


async def probe_run(factory, ks) -> None:
    for k in ks:
        sc = factory()
        await asyncio.wait_for(sc.setup(), 12)
        started = time.monotonic()
        try:
            tr = await _await_target(sc.target(), k, 12)
            surv = await bound(sc.survivor(), 8)
            obs = await bound(sc.observe(), 8)
            print(
                f"PROBE k={k} target={tr.outcome} realized={tr.realized} yields={tr.yields} "
                f"err={tr.error!r} diag={sc.diag!r}\n"
                f"        stages={sc.diag.get('stages')!r} "
                f"after_target={sc.diag.get('after_target')!r}\n"
                f"        survivor={surv!r}\n        observer={obs!r} "
                f"seconds={time.monotonic() - started:.2f}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument("--diag", action="store_true")
    args = parser.parse_args()
    print(
        f"case=aiomysql mode={args.mode} aiomysql={aiomysql.__version__} "
        f"python={__import__('sys').version.split()[0]} host={args.host} "
        f"port={args.port} db={args.dbname} round={args.round} "
        f"delay={DELAY} bound={BOUND}",
        flush=True,
    )
    factory = lambda: AiomysqlScenario(args)
    if args.probe:
        await probe_run(factory, [int(x) for x in args.probe.split(",") if x.strip()])
        return
    if args.diag:
        await diag_run(factory)
        return

    timeout = 12.0
    reports = []
    for level in [x.strip() for x in args.levels.split(",") if x.strip()]:
        if level == "B1":
            r = await level_b1("aiomysql", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "aiomysql", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "aiomysql", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed("aiomysql", factory, reps=args.reps, timeout=timeout)
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
