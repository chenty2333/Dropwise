"""SQLAlchemy #6592 + asyncpg: interruption around a transaction.

Two interruption models, because #6592 is not a cancellation bug on Python
3.8+:

  ladder        ``Task.cancel()`` at a suspension, the same model as the
                redis/aiohttp cases.  In this scenario many early cuts leave
                SQLAlchemy's greenlet bridge stuck (survivor blocks), so the
                driver can run an explicit subset of plans (``--plans``).
  timeout-begin  the issue's own fault injection: asyncpg's client-side
                ``timeout=0`` on the BEGIN issued inside ``engine.begin()``.
                This makes ``asyncio.TimeoutError`` reach SQLAlchemy's
                ``_handle_dbapi_exception``, which is the path 1.4.18 added to
                ``is_exit_exception``.  The patch is in-process only, applied
                around the target and removed afterwards; no file is modified.

The pool is ``pool_size=1, max_overflow=0``.  The survivor updates the same row
through the same engine and reads it back; a direct asyncpg connection (not the
SQLAlchemy pool) reads the row as the independent observer.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
from urllib.parse import urlparse

import asyncpg
from sqlalchemy import text
from sqlalchemy.ext.asyncio import create_async_engine

from dropwise_py import (
    Expect,
    Scenario,
    Trial,
    Verdict,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
    report_lines,
)

from .common import compact

KEY = "dropwise:py:row"
SLEEP = 0.4

_ORIGINAL_EXECUTE = asyncpg.Connection.execute
_PATCH = {"active": False}


async def _patched_execute(self, query, *args, timeout=None):
    if _PATCH["active"] and str(query).lstrip().upper().startswith("BEGIN"):
        timeout = 0  # asyncpg raises asyncio.TimeoutError before sending BEGIN
    return await _ORIGINAL_EXECUTE(self, query, *args, timeout=timeout)


# Runtime-only hook; inert unless _PATCH["active"] is set around a target.
asyncpg.Connection.execute = _patched_execute


def pg_dsn(sqla_url: str) -> str:
    parsed = urlparse(sqla_url)
    if not parsed.scheme.startswith("postgresql"):
        raise SystemExit(f"not a postgresql+asyncpg URL: {sqla_url}")
    return sqla_url.replace("postgresql+asyncpg://", "postgresql://", 1)


def make_engine(url: str):
    return create_async_engine(url, pool_size=1, max_overflow=0)


async def reset(observer: asyncpg.Connection) -> None:
    await observer.execute(
        "CREATE TABLE IF NOT EXISTS dw_kv (id text primary key, v int not null)"
    )
    await observer.execute(
        "INSERT INTO dw_kv (id, v) VALUES ($1, 0) "
        "ON CONFLICT (id) DO UPDATE SET v = 0",
        KEY,
    )


class SqlaScenario(Scenario):
    def __init__(self, url: str, inject_begin_timeout: bool = False) -> None:
        self.url = url
        self.inject_begin_timeout = inject_begin_timeout
        self.engine = None
        self.observer: asyncpg.Connection | None = None

    async def setup(self) -> None:
        self.observer = await asyncpg.connect(pg_dsn(self.url))
        await reset(self.observer)
        self.engine = make_engine(self.url)
        # Warm up the dialect and leave one checked-in connection, so the
        # fault injection hits the target's BEGIN, not connect-time dialect
        # initialization (that initialization is outside the transaction
        # block in the issue's repro too).
        async with self.engine.begin() as conn:
            await conn.execute(text("SELECT 1"))

    async def target(self):
        if self.inject_begin_timeout:
            _PATCH["active"] = True
            try:
                async with self.engine.begin() as conn:
                    try:
                        await conn.execute(
                            text("UPDATE dw_kv SET v = 1 WHERE id = :k"), {"k": KEY}
                        )
                    except asyncio.TimeoutError:
                        # Issue #6592's shape: the DBAPI timeout happens while
                        # BEGIN is being issued, and the transaction block
                        # catches it and rolls back.
                        await conn.rollback()
            finally:
                _PATCH["active"] = False
            return
        async with self.engine.begin() as conn:
            await conn.execute(text("UPDATE dw_kv SET v = 1 WHERE id = :k"), {"k": KEY})
            await conn.execute(text("SELECT pg_sleep(:s)"), {"s": SLEEP})

    async def same(self):
        # B2 retries the same operation without injecting the fault again.
        if self.inject_begin_timeout:
            self.inject_begin_timeout = False
            try:
                return await self.target()
            finally:
                self.inject_begin_timeout = True
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
        return await self.observer.fetchval("SELECT v FROM dw_kv WHERE id = $1", KEY)

    async def close(self) -> None:
        if self.engine is not None:
            with contextlib.suppress(BaseException):
                await self.engine.dispose()
        if self.observer is not None:
            with contextlib.suppress(BaseException):
                await self.observer.close()


async def _observation(factory, *, skip_target: bool = False):
    sc = factory()
    await sc.setup()
    try:
        target_error = None
        if not skip_target:
            try:
                await asyncio.wait_for(sc.target(), 10)
            except BaseException as exc:
                target_error = f"{type(exc).__name__}: {exc}"
        try:
            survivor = await asyncio.wait_for(sc.survivor(), 10)
        except BaseException as exc:
            survivor = f"ERR {type(exc).__name__}: {exc}"
        try:
            observer = await asyncio.wait_for(sc.observe(), 10)
        except BaseException as exc:
            observer = f"ERR {type(exc).__name__}: {exc}"
        return {"survivor": survivor, "observer": observer}, target_error
    finally:
        await sc.close()


async def fault_levels(url: str, reps: int, levels: list[str]) -> None:
    """Four levels around the timeout-in-BEGIN fault injection."""
    factory = lambda: SqlaScenario(url, inject_begin_timeout=True)
    pre = [await _observation(factory, skip_target=True) for _ in range(2)]
    post = [await _observation(lambda: SqlaScenario(url, False)) for _ in range(2)]
    pre_obs = [x[0] for x in pre]
    post_obs = [x[0] for x in post]
    calibration_ok = pre_obs[0] == pre_obs[1] and post_obs[0] == post_obs[1]
    print(f"timeout_begin_pre={pre_obs!r}", flush=True)
    print(f"timeout_begin_post={post_obs!r}", flush=True)
    print(f"calibration_ok={calibration_ok}", flush=True)

    # B1: only the interrupted target; its own TimeoutError is caught inside.
    for rep in range(1, reps + 1) if "B1" in levels else ():
        sc = factory()
        await sc.setup()
        try:
            target_error = None
            try:
                await asyncio.wait_for(sc.target(), 10)
            except BaseException as exc:
                target_error = f"{type(exc).__name__}: {exc}"
            status = "VIOLATION" if target_error else "ok"
            print(
                f"  level=B1 plan=begin-timeout rep={rep} "
                f"target={target_error or 'interrupted-caught'} {status}",
                flush=True,
            )
        finally:
            await sc.close()

    # B2/B3: retry the same target / a different operation after the fault.
    for level, retry in ((x, y) for x, y in (("B2", "same"), ("B3", "different")) if x in levels):
        for rep in range(1, reps + 1):
            sc = factory()
            await sc.setup()
            try:
                target_error = None
                try:
                    await asyncio.wait_for(sc.target(), 10)
                except BaseException as exc:
                    target_error = f"{type(exc).__name__}: {exc}"
                op = sc.same() if retry == "same" else sc.different()
                try:
                    value = await asyncio.wait_for(op, 10)
                    value_repr = repr(value)
                    violation = None
                except BaseException as exc:
                    value_repr = f"ERR {type(exc).__name__}: {exc}"
                    violation = value_repr
                print(
                    f"  level={level} plan=begin-timeout rep={rep} target={target_error or 'caught'} "
                    f"obs={value_repr} {'VIOLATION ' + violation if violation else 'ok'}",
                    flush=True,
                )
            finally:
                await sc.close()

    # M1: survivor + independent observer.
    for rep in range(1, reps + 1) if "M1" in levels else ():
        obs, target_error = await _observation(factory)
        if not calibration_ok:
            verdict = Verdict.UNKNOWN
        elif obs in pre_obs:
            verdict = Verdict.PRE
        elif obs in post_obs:
            verdict = Verdict.POST
        else:
            verdict = Verdict.NEITHER
        violation = None
        if verdict is Verdict.NEITHER:
            violation = f"Neither: observed {obs!r}, pre={pre_obs[0]!r}, post={post_obs[0]!r}"
        print(
            f"  level=M1 plan=begin-timeout rep={rep} target={target_error or 'caught'} "
            f"verdict={verdict.value} obs={obs!r} "
            f"{'VIOLATION ' + violation if violation else 'ok'}",
            flush=True,
        )

    print("=== FINAL ===", flush=True)
    for level in levels:
        print(f"SUMMARY level={level} plan=begin-timeout reps={reps}", flush=True)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="SQLAlchemy asyncpg URL")
    parser.add_argument("--mode", choices=("ladder", "timeout-begin"), default="ladder")
    parser.add_argument("--levels", default="B1,B2,B3,M1")
    parser.add_argument("--plans", default="2,3,4,5,6", help="ladder plan subset")
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--round", type=int, default=0)
    args = parser.parse_args()
    import sqlalchemy

    print(
        f"case=sqlalchemy version={sqlalchemy.__version__} mode={args.mode} "
        f"round={args.round} plans={args.plans} url={args.url}",
        flush=True,
    )
    if args.mode == "timeout-begin":
        levels = [x.strip() for x in args.levels.split(",") if x.strip()]
        await fault_levels(args.url, args.reps, levels)
        return

    plans = [int(x) for x in args.plans.split(",") if x.strip()]
    factory = lambda: SqlaScenario(args.url)
    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1(
                "sqlalchemy", factory, reps=args.reps, timeout=args.timeout, plans=plans
            )
        elif level == "B2":
            r = await level_retry(
                "sqlalchemy",
                factory,
                level="B2",
                retry="same",
                reps=args.reps,
                timeout=args.timeout,
                plans=plans,
            )
        elif level == "B3":
            r = await level_retry(
                "sqlalchemy",
                factory,
                level="B3",
                retry="different",
                reps=args.reps,
                timeout=args.timeout,
                plans=plans,
            )
        elif level == "M1":
            r = await explore_observed(
                "sqlalchemy",
                factory,
                reps=args.reps,
                timeout=args.timeout,
                plans=plans,
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
