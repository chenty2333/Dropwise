"""SQLAlchemy 2.x triage case: pure ``Task.cancel()`` around ``engine.begin()``.

Same judging shape as ``sqla_case.py`` (warm engine, pool_size=1, survivor
through the same engine, independent observer outside the pool), but:

* the URL may use either async driver: ``postgresql+asyncpg`` or
  ``postgresql+psycopg`` (psycopg 3 async);
* the observer uses the same driver family as the target, on its own raw
  connection, so a driver-specific observation path cannot hide a difference;
* every invocation covers one plan (the runner uses a process per plan, so a
  hang is isolated and can be killed).

This module is triage code only; it does not modify the installed packages.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib

import asyncpg
import psycopg
from sqlalchemy import make_url, text
from sqlalchemy.ext.asyncio import create_async_engine

from dropwise_py import (
    Expect,
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)

from .common import compact

KEY = "dropwise:py:row"
SLEEP = 0.4


def observer_dsn(url: str) -> str:
    for prefix in (
        "postgresql+asyncpg://",
        "postgresql+psycopg://",
        "postgresql+psycopg_async://",
    ):
        if url.startswith(prefix):
            return "postgresql://" + url[len(prefix) :]
    raise SystemExit(f"not a postgresql URL: {url}")


class CurrentScenario(Scenario):
    def __init__(self, url: str) -> None:
        self.url = url
        parsed = make_url(url)
        self.driver = parsed.get_driver_name()
        self.engine = None
        self.observer = None

    async def setup(self) -> None:
        dsn = observer_dsn(self.url)
        if self.driver == "asyncpg":
            self.observer = await asyncpg.connect(dsn)
            await self.observer.execute(
                "CREATE TABLE IF NOT EXISTS dw_kv "
                "(id text primary key, v int not null)"
            )
            await self.observer.execute(
                "INSERT INTO dw_kv (id, v) VALUES ('" + KEY + "', 0) "
                "ON CONFLICT (id) DO UPDATE SET v = 0"
            )
        elif self.driver in ("psycopg", "psycopg_async"):
            self.observer = await psycopg.AsyncConnection.connect(
                dsn, autocommit=True
            )
            async with self.observer.cursor() as cur:
                await cur.execute(
                    "CREATE TABLE IF NOT EXISTS dw_kv "
                    "(id text primary key, v int not null)"
                )
                await cur.execute(
                    "INSERT INTO dw_kv (id, v) VALUES ('" + KEY + "', 0) "
                    "ON CONFLICT (id) DO UPDATE SET v = 0"
                )
        else:
            raise SystemExit(f"unsupported driver {self.driver!r}")

        self.engine = create_async_engine(
            self.url, pool_size=1, max_overflow=0
        )
        # Warm the dialect and leave one checked-in connection, so a cut in
        # the target's BEGIN is not a cut in connect-time initialization.
        async with self.engine.begin() as conn:
            await conn.execute(text("SELECT 1"))

    async def target(self) -> None:
        async with self.engine.begin() as conn:
            await conn.execute(
                text("UPDATE dw_kv SET v = 1 WHERE id = :k"), {"k": KEY}
            )
            await conn.execute(text("SELECT pg_sleep(:s)"), {"s": SLEEP})

    async def same(self):
        return await self.target()

    async def different(self):
        async with self.engine.begin() as conn:
            result = await conn.execute(
                text("UPDATE dw_kv SET v = 8 WHERE id = :k"), {"k": KEY}
            )
            return result.rowcount

    async def survivor(self):
        try:
            async with self.engine.begin() as conn:
                await conn.execute(
                    text("UPDATE dw_kv SET v = 7 WHERE id = :k"), {"k": KEY}
                )
            write = "ok"
        except BaseException as exc:
            write = f"ERR {type(exc).__name__}: {exc}"
        try:
            async with self.engine.connect() as conn:
                own = (
                    await conn.execute(
                        text("SELECT v FROM dw_kv WHERE id = :k"), {"k": KEY}
                    )
                ).scalar()
        except BaseException as exc:
            own = f"ERR {type(exc).__name__}: {exc}"
        return {"write": write, "own": own}

    async def observe(self):
        if self.driver == "asyncpg":
            return await self.observer.fetchval(
                "SELECT v FROM dw_kv WHERE id = $1", KEY
            )
        async with self.observer.cursor() as cur:
            await cur.execute(
                "SELECT v FROM dw_kv WHERE id = %s", (KEY,)
            )
            row = await cur.fetchone()
            return row[0]

    async def close(self) -> None:
        if self.engine is not None:
            with contextlib.suppress(BaseException):
                await self.engine.dispose()
        if self.observer is not None:
            with contextlib.suppress(BaseException):
                await self.observer.close()


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="SQLAlchemy async URL")
    parser.add_argument("--levels", default="M1")
    parser.add_argument("--plans", default="1,2,3,4,5,6,7")
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--round", type=int, default=0)
    args = parser.parse_args()
    import sqlalchemy

    plans = [int(x) for x in args.plans.split(",") if x.strip()]
    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    factory = lambda: CurrentScenario(args.url)
    print(
        f"case=sqlalchemy-current version={sqlalchemy.__version__} "
        f"driver={make_url(args.url).get_driver_name()} round={args.round} "
        f"plans={args.plans} url={args.url}",
        flush=True,
    )
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1(
                "sqlalchemy-current", factory, reps=args.reps,
                timeout=args.timeout, plans=plans,
            )
        elif level == "B2":
            r = await level_retry(
                "sqlalchemy-current", factory, level="B2", retry="same",
                reps=args.reps, timeout=args.timeout, plans=plans,
            )
        elif level == "B3":
            r = await level_retry(
                "sqlalchemy-current", factory, level="B3", retry="different",
                reps=args.reps, timeout=args.timeout, plans=plans,
            )
        elif level == "M1":
            r = await explore_observed(
                "sqlalchemy-current", factory, reps=args.reps,
                timeout=args.timeout, plans=plans,
            )
        else:
            raise SystemExit(f"unknown level {level}")
        print_report(r)
        print(compact(r), flush=True)
        reports.append(r)
    print("=== FINAL ===", flush=True)
    for r in reports:
        print(compact(r), flush=True)
        print(f"violations_by_plan={[t.plan for t in r.violations()]}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
