"""aiohttp #12795: can cancelling a request poison the keep-alive pool?

The server is an in-process ``aiohttp.web`` app on 127.0.0.1.  The target is a
streamed POST that sets a key and then dribbles its response body; cancelling
while the body is being read is the case the 3.14.1 ``except BaseException``
change is supposed to cover.  The survivor posts to the same key through the
same one-connection ``TCPConnector``; a separate session reads the server-side
value as the independent observer.

Two modes:
  ladder  B1/B2/B3/M1 cancellation ladder.
  idle    supplementary diagnostic for the issue's real ~2 ms signature: a
          completed response's pooled connection keeps a re-armed sock_read
          timer; not a cancellation result and not counted as a violation.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import time

import aiohttp
from aiohttp import ClientSession, ClientTimeout, TCPConnector, web
from aiohttp.client_proto import ResponseHandler

from dropwise_py import (
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)

from .common import compact

KEY = "dropwise:py:http"
STATE: dict[str, str] = {}
INSTRUMENT = {"calls": 0, "baseexc": [], "while_cancelling": 0, "serving": False}


def instrument_data_received() -> None:
    original = ResponseHandler.data_received

    def wrapped(self, data):  # type: ignore[no-untyped-def]
        INSTRUMENT["calls"] += 1
        task = asyncio.current_task()
        if task is not None and getattr(task, "cancelling", lambda: 0)():
            INSTRUMENT["while_cancelling"] += 1
        try:
            return original(self, data)
        except BaseException as exc:
            INSTRUMENT["baseexc"].append(type(exc).__name__)
            raise

    ResponseHandler.data_received = wrapped


async def slow_set(request: web.Request) -> web.StreamResponse:
    key = request.query["key"]
    value = request.query["value"]
    STATE[key] = value
    resp = web.StreamResponse()
    resp.content_type = "text/plain"
    await resp.prepare(request)
    try:
        for piece in (value, "-tail"):
            await asyncio.sleep(0.05)
            await resp.write(piece.encode())
        await resp.write_eof()
    except (ConnectionResetError, asyncio.CancelledError):
        pass
    return resp


async def set_value(request: web.Request) -> web.Response:
    key = request.query["key"]
    value = request.query["value"]
    STATE[key] = value
    return web.Response(text=value)


async def get_value(request: web.Request) -> web.Response:
    return web.Response(text=STATE.get(request.query["key"], ""))


async def fast(request: web.Request) -> web.Response:
    return web.Response(text="fast")


async def start_server() -> tuple[web.AppRunner, str]:
    app = web.Application()
    app.router.add_post("/slow_set", slow_set)
    app.router.add_post("/set", set_value)
    app.router.add_get("/get", get_value)
    app.router.add_get("/fast", fast)
    runner = web.AppRunner(app, access_log=None)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    assert site._server is not None
    port = site._server.sockets[0].getsockname()[1]
    return runner, f"http://127.0.0.1:{port}"


class HttpScenario(Scenario):
    def __init__(self, base: str) -> None:
        self.base = base
        self.session: ClientSession | None = None
        self.observer_session: ClientSession | None = None

    async def setup(self) -> None:
        STATE[KEY] = "initial"
        connector = TCPConnector(limit=1, limit_per_host=1)
        self.session = ClientSession(connector=connector)
        self.observer_session = ClientSession()

    async def target(self):
        async with self.session.post(
            f"{self.base}/slow_set", params={"key": KEY, "value": "target"}
        ) as resp:
            return await resp.text()

    async def same(self):
        return await self.target()

    async def different(self):
        async with self.session.get(f"{self.base}/get", params={"key": KEY}) as resp:
            return await resp.text()

    async def survivor(self):
        async with self.session.post(
            f"{self.base}/set", params={"key": KEY, "value": "survivor"}
        ) as resp:
            body = await resp.text()
        return {"status": resp.status, "body": body}

    async def observe(self):
        async with self.observer_session.get(
            f"{self.base}/get", params={"key": KEY}
        ) as resp:
            return await resp.text()

    async def close(self) -> None:
        for session in (self.session, self.observer_session):
            if session is not None:
                with contextlib.suppress(BaseException):
                    await session.close()


async def run_ladder(base: str, args: argparse.Namespace) -> None:
    factory = lambda: HttpScenario(base)
    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1("aiohttp", factory, reps=args.reps, timeout=args.timeout)
        elif level == "B2":
            r = await level_retry(
                "aiohttp", factory, level="B2", retry="same", reps=args.reps, timeout=args.timeout
            )
        elif level == "B3":
            r = await level_retry(
                "aiohttp", factory, level="B3", retry="different", reps=args.reps, timeout=args.timeout
            )
        elif level == "M1":
            r = await explore_observed(
                "aiohttp", factory, reps=args.reps, timeout=args.timeout
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


async def run_idle(base: str) -> None:
    result: dict[str, object] = {"aiohttp": aiohttp.__version__}
    connector = TCPConnector(limit=1, limit_per_host=1)
    session = ClientSession(
        connector=connector, timeout=ClientTimeout(total=30, sock_read=0.1)
    )
    try:
        async with session.get(base + "/fast") as resp:
            await resp.read()
        pooled = list(connector._conns.values())[0][0][0]
        result["pooled_after_normal_read"] = {
            "read_timeout_handle_armed": pooled._read_timeout_handle is not None,
            "exception": repr(pooled.exception()) if pooled.exception() else None,
            "connected": pooled.is_connected(),
        }
        await asyncio.sleep(0.15)
        result["pooled_after_idle"] = {
            "read_timeout_handle_armed": pooled._read_timeout_handle is not None,
            "exception": repr(pooled.exception()) if pooled.exception() else None,
            "connected": pooled.is_connected(),
            "should_close": pooled.should_close,
        }
        for name in ("survivor_1", "survivor_2"):
            t0 = time.perf_counter()
            try:
                async with session.get(base + "/fast") as resp:
                    body = await resp.read()
                result[name] = {
                    "ok": True,
                    "body": body.decode(),
                    "ms": round((time.perf_counter() - t0) * 1000, 2),
                }
            except BaseException as exc:
                result[name] = {
                    "ok": False,
                    "exc": type(exc).__name__,
                    "msg": str(exc)[:160],
                    "ms": round((time.perf_counter() - t0) * 1000, 2),
                }
    finally:
        await session.close()
    print(json.dumps(result, sort_keys=True), flush=True)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("ladder", "idle"), default="ladder")
    parser.add_argument("--levels", default="B1,B2,B3,M1")
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--round", type=int, default=0)
    parser.add_argument("--instrument", action="store_true")
    args = parser.parse_args()
    if args.instrument:
        instrument_data_received()

    runner, base = await start_server()
    try:
        print(
            f"case=aiohttp mode={args.mode} version={aiohttp.__version__} "
            f"round={args.round} base={base}",
            flush=True,
        )
        if args.mode == "idle":
            await run_idle(base)
        else:
            await run_ladder(base, args)
        if args.instrument:
            print(f"instrument={json.dumps(INSTRUMENT, sort_keys=True)}", flush=True)
    finally:
        await runner.cleanup()


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
