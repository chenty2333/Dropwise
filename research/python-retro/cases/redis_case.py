"""redis-py cancellation cases.

Two target shapes, because the two CVE fixes cover different code paths:

  blpop     BLPOP key 0.5, a non-pipeline command whose reply is pending while
            the server blocks.  Fixed by #2666 in 4.5.4 (CVE-2023-28859).
  pipeline  MULTI; SET; GET; EXEC, fixed by #2641 in 4.5.3 (CVE-2023-28858).

The `blpop` target needs no artificial delay: the server itself holds the reply
until the timeout or until the survivor pushes a value, so the unread reply is
deterministic.  The pool has one connection; the survivor pushes to the same
key through that pool, reads it back through the same pool, and an independent
client (direct to Redis, not through the tested pool) lists the key.
"""

from __future__ import annotations

import asyncio
import contextlib
from urllib.parse import urlparse

from redis.asyncio import BlockingConnectionPool, Redis

from dropwise_py import (
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)

from .common import DelayProxy, base_parser, compact

KEY = "dropwise:py:list"
TARGET = b"target"
SURVIVOR = b"survivor"
DIFFERENT = b"b3"
BLPOP_TIMEOUT = 0.5


def parse_redis_url(url: str) -> tuple[str, int, int]:
    p = urlparse(url)
    if p.scheme not in ("redis", "rediss"):
        raise SystemExit(f"not a redis URL: {url}")
    return p.hostname or "127.0.0.1", p.port or 6379, int((p.path or "/0").lstrip("/") or 0)


class RedisScenario(Scenario):
    def __init__(self, host: str, port: int, db: int, target: str) -> None:
        self.host = host
        self.port = port
        self.db = db
        self.target_name = target
        self.proxy: DelayProxy | None = None
        self.pool = None
        self.client = None
        self.observer = None

    async def setup(self) -> None:
        client_port = self.port
        if self.target_name in ("pipeline", "get"):
            # These ordinary commands complete before the cancellation point
            # unless the reply is delayed; keep them for the secondary probe.
            self.proxy = DelayProxy(self.host, self.port, delay=0.30)
            client_port = await self.proxy.start()
        self.pool = BlockingConnectionPool(
            host="127.0.0.1",
            port=client_port,
            db=self.db,
            max_connections=1,
            timeout=5,
            decode_responses=False,
        )
        self.client = Redis(connection_pool=self.pool)
        self.observer = Redis(
            host=self.host, port=self.port, db=self.db, decode_responses=False
        )
        await self.observer.delete(KEY)

    async def target(self):
        if self.target_name == "pipeline":
            async with self.client.pipeline(transaction=True) as pipe:
                pipe.set(KEY, TARGET)
                pipe.get(KEY)
                return await pipe.execute()
        if self.target_name == "get":
            return await self.client.get(KEY)
        return await self.client.blpop(KEY, timeout=BLPOP_TIMEOUT)

    async def same(self):
        return await self.target()

    async def different(self):
        if self.target_name in ("pipeline", "get"):
            return await self.client.set(KEY, DIFFERENT)
        return await self.client.rpush(KEY, DIFFERENT)

    async def survivor(self):
        if self.target_name in ("pipeline", "get"):
            try:
                write = await self.client.set(KEY, SURVIVOR)
            except BaseException as exc:
                write = f"ERR {type(exc).__name__}: {exc}"
            try:
                own = await self.client.get(KEY)
            except BaseException as exc:
                own = f"ERR {type(exc).__name__}: {exc}"
            return {"write": write, "own": own}
        try:
            write = await self.client.rpush(KEY, SURVIVOR)
        except BaseException as exc:
            write = f"ERR {type(exc).__name__}: {exc}"
        try:
            own = await self.client.lrange(KEY, 0, -1)
        except BaseException as exc:
            own = f"ERR {type(exc).__name__}: {exc}"
        return {"write": write, "own": own}

    async def observe(self):
        if self.target_name in ("pipeline", "get"):
            return await self.observer.get(KEY)
        return await self.observer.lrange(KEY, 0, -1)

    async def close(self) -> None:
        for closer in (
            lambda: self.client.close(close_connection_pool=False),
            lambda: self.observer.close(),
            lambda: self.pool.disconnect(),
        ):
            with contextlib.suppress(BaseException):
                await closer()
        if self.proxy is not None:
            await self.proxy.close()


async def main() -> None:
    args = base_parser(__doc__).parse_args()
    host, port, db = parse_redis_url(args.url)
    target = args.target or "blpop"
    if target not in ("blpop", "pipeline", "get"):
        raise SystemExit("--target must be blpop, pipeline or get")
    factory = lambda: RedisScenario(host, port, db, target)
    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    import redis

    print(
        f"case=redis target={target} version={redis.__version__} "
        f"url={args.url} round={args.round}",
        flush=True,
    )

    async def run_level(level: str):
        if level == "B1":
            r = await level_b1("redis", factory, reps=args.reps, timeout=args.timeout)
        elif level == "B2":
            r = await level_retry(
                "redis", factory, level="B2", retry="same", reps=args.reps, timeout=args.timeout
            )
        elif level == "B3":
            r = await level_retry(
                "redis", factory, level="B3", retry="different", reps=args.reps, timeout=args.timeout
            )
        elif level == "M1":
            r = await explore_observed(
                "redis", factory, reps=args.reps, timeout=args.timeout
            )
        else:
            raise SystemExit(f"unknown level {level}")
        print_report(r)
        print(compact(r), flush=True)
        return r

    reports = []
    for level in levels:
        reports.append(await run_level(level))
    print("=== FINAL ===", flush=True)
    for r in reports:
        print(compact(r), flush=True)
        print(f"violations_by_plan={[t.plan for t in r.violations()]}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
