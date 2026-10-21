"""asyncpg 0.32.0 cancellation cases (pool and direct).

Same skeleton as the psycopg case: target = BEGIN / upsert(target) / COMMIT on
one checked-out connection, survivor = upsert(survivor) + same-connection
readback, observer = independent direct connection.  asyncpg has no
`autocommit` switch: `execute()` outside a transaction block is its own
server-side transaction, so a survivor write that lands inside a leftover
BEGIN block is silently uncommitted.

Pending: retro DelayProxy, 50 ms per server->client chunk, only around the
tested connection.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import time

import asyncpg

from .common import (  # also puts research/python-retro on sys.path
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
from dropwise_py.engine import _await_target

TARGET_ROW = "target"
SURVIVOR_ROW = "survivor"
B3_ROW = "b3"


def dsn(host: str, port: int, user: str, password: str, dbname: str) -> str:
    return f"postgresql://{user}:{password}@{host}:{port}/{dbname}"


class AsyncpgScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.table = unique_table("dw_r3_apg")
        self.proxy: DelayProxy | None = None
        self.pool: asyncpg.Pool | None = None
        self.conn: asyncpg.Connection | None = None
        self.observer: asyncpg.Connection | None = None
        self.diag: dict = {}

    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        direct = dsn(a.host, a.port, a.user, a.password, a.dbname)
        via_proxy = dsn(a.host, client_port, a.user, a.password, a.dbname)

        obs = await bound(asyncpg.connect(direct, timeout=BOUND))
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
            pool = await bound(
                asyncpg.create_pool(via_proxy, min_size=1, max_size=1, timeout=BOUND)
            )
            if pool[0] != "ok":
                raise RuntimeError(f"pool create failed: {pool}")
            self.pool = pool[1]
            warm = await bound(self._pool_exec("SELECT 1"))
            if warm[0] != "ok":
                raise RuntimeError(f"warm-up failed: {warm}")
            self.diag["setup_pid"] = await self._pid_pool()
        else:
            conn = await bound(asyncpg.connect(via_proxy, timeout=BOUND))
            if conn[0] != "ok":
                raise RuntimeError(f"tested connect failed: {conn}")
            self.conn = conn[1]
            warm = await bound(self.conn.execute("SELECT 1"))
            if warm[0] != "ok":
                raise RuntimeError(f"warm-up failed: {warm}")
            self.diag["setup_pid"] = self.conn.get_server_pid()
        self.diag["setup_stats"] = self._stats()

    async def _pool_exec(self, sql: str):
        conn = await self.pool.acquire(timeout=BOUND)
        try:
            return await conn.execute(sql)
        finally:
            await self.pool.release(conn)

    async def _pid_pool(self):
        try:
            conn = await self.pool.acquire(timeout=BOUND)
        except BaseException:
            return None
        try:
            return conn.get_server_pid()
        finally:
            await self.pool.release(conn)

    def _stats(self):
        if self.pool is None:
            return None
        return {
            "size": self.pool.get_size(),
            "idle": self.pool.get_idle_size(),
            "min": self.pool.get_min_size(),
            "max": self.pool.get_max_size(),
        }

    async def _acquire(self):
        if self.mode == "pool":
            return await self.pool.acquire(timeout=BOUND)
        return self.conn

    async def _release(self, conn) -> None:
        if self.mode == "pool" and conn is not None:
            await self.pool.release(conn)

    async def _tx_write(self, row: str, value: int):
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return {"acquire": acq}
        conn = acq[1]
        self.diag["stages"] = stages = []
        self.diag["target_pid"] = conn.get_server_pid()
        try:
            await bound(conn.execute("BEGIN"))
            stages.append("begin")
            await bound(
                conn.execute(
                    f"INSERT INTO {self.table} (k, v) VALUES ('{row}', {value}) "
                    "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v"
                )
            )
            stages.append("insert")
            await bound(conn.execute("COMMIT"))
            stages.append("commit")
        finally:
            self.diag["after_target"] = {
                "in_transaction": conn.is_in_transaction(),
                "pid": conn.get_server_pid(),
                "stats": self._stats(),
            }
            rel = await bound(self._release(conn))
            self.diag["after_release"] = {"stats": self._stats(), "release": rel}

    async def target(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def same(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def different(self):
        return await self._tx_write(B3_ROW, 3)

    async def survivor(self):
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return {"acquire": acq, "in_tx": None, "write": None, "own": None, "release": None}
        conn = acq[1]
        in_tx = conn.is_in_transaction()
        self.diag["survivor_start"] = {
            "in_transaction": in_tx,
            "pid": conn.get_server_pid(),
            "stats": self._stats(),
        }
        write = await bound(
            conn.execute(
                f"INSERT INTO {self.table} (k, v) VALUES ('{SURVIVOR_ROW}', 7) "
                "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v"
            )
        )
        own = None
        if write[0] != "timeout":
            own = await bound(
                conn.fetchval(f"SELECT v FROM {self.table} WHERE k = '{SURVIVOR_ROW}'")
            )
        release = await bound(self._release(conn))
        self.diag["survivor_end"] = {"stats": self._stats()}
        return {"in_tx": in_tx, "write": write, "own": own, "release": release}

    async def observe(self):
        rows = await self.observer.fetch(f"SELECT k, v FROM {self.table} ORDER BY k")
        return {"rows": [[r["k"], r["v"]] for r in rows], "error": None}

    async def close(self) -> None:
        if self.pool is not None:
            await close_quiet(self.pool.close())
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
                f"PROBE k={k} target={tr.outcome} realized={tr.realized} "
                f"yields={tr.yields} err={tr.error!r}\n"
                f"        stages={sc.diag.get('stages')!r} after_target={sc.diag.get('after_target')!r}\n"
                f"        survivor={surv!r}\n"
                f"        observer={obs!r} seconds={time.monotonic() - started:.2f}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        print(f"DIAG setup pid={sc.diag.get('setup_pid')} stats={sc._stats()}", flush=True)
        pre = await sc.judged()
        print(f"DIAG pre obs={pre!r}", flush=True)
        tr = await _await_target(sc.target(), None, 12)
        print(f"DIAG post target={tr.outcome}/{tr.yields} diag={sc.diag!r}", flush=True)
        post = await sc.judged()
        print(f"DIAG post obs={post!r}", flush=True)
        print(f"DIAG pre==post {pre == post}", flush=True)
    finally:
        await close_quiet(sc.close())


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument("--diag", action="store_true")
    args = parser.parse_args()
    print(
        f"case=asyncpg mode={args.mode} asyncpg={asyncpg.__version__} "
        f"python={__import__('sys').version.split()[0]} host={args.host} "
        f"port={args.port} db={args.dbname} round={args.round} "
        f"delay={DELAY} bound={BOUND}",
        flush=True,
    )
    factory = lambda: AsyncpgScenario(args)
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
            r = await level_b1("asyncpg", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "asyncpg", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "asyncpg", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed("asyncpg", factory, reps=args.reps, timeout=timeout)
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
