#!/usr/bin/env python3
"""Triage helper for the httpx cancellation point and the pool state after it.

Not part of the level ladder.  It runs one uncancelled target to count yields,
then one cancelled target, printing the coroutine stack after every yield and
the httpcore pool state before/after a bounded retry.  This is the "shrink to
the plan/stage" step; the client source was read only after M1 showed
pending#6.

Usage:
  python -m cases.triage_httpx --url http://127.0.0.1:<port> --k 6
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import traceback
from urllib.parse import urlparse

import httpx

from .support import DelayProxy, BOUND, unique  # also puts research/python-retro on sys.path

from dropwise_py.engine import _CutTask  # noqa: E402


class TracingCutTask(_CutTask):
    def __init__(self, coro, *, cut_after):
        self.trace = []
        super().__init__(coro, cut_after=cut_after)

    def _Task__step(self, exc=None) -> None:
        before = self.yields
        super()._Task__step(exc)
        if not self.done() and self.yields != before:
            frames = _frame_chain(self.get_coro())
            self.trace.append((self.yields, frames))
            print(f"YIELD {self.yields}: " + " <- ".join(frames), flush=True)


def _frame_chain(coro) -> list[str]:
    """Walk the suspended coroutine/await chain to name each await site."""
    frames = []
    seen = 0
    while coro is not None and seen < 24:
        frame = (
            getattr(coro, "cr_frame", None)
            or getattr(coro, "gi_frame", None)
            or getattr(coro, "ag_frame", None)
        )
        if frame is not None:
            frames.append(
                f"{frame.f_code.co_filename.split('/')[-1]}:{frame.f_lineno} "
                f"{frame.f_code.co_name}"
            )
        coro = (
            getattr(coro, "cr_await", None)
            or getattr(coro, "gi_yieldfrom", None)
            or getattr(coro, "ag_await", None)
        )
        seen += 1
    return frames


def pool_info(client: httpx.AsyncClient) -> str:
    pool = client._transport._pool  # noqa: SLF001 - triage only
    conns = [str(conn.info()) for conn in pool.connections]
    return f"connections={conns} requests={len(pool._requests)}"


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--k", type=int, default=6)
    parser.add_argument(
        "--mode",
        choices=("yield", "trigger"),
        default="yield",
        help="yield: engine-style cancel after k yields; trigger: cancel a plain "
        "C task as soon as the server log shows the target request",
    )
    parser.add_argument("--trigger-sleep", type=float, default=0.08)
    parser.add_argument("--split-body", type=float, default=0.5)
    args = parser.parse_args()
    parsed = urlparse(args.url)
    proxy = DelayProxy(parsed.hostname, parsed.port, delay=0.05)
    proxy_port = await proxy.start()
    client = httpx.AsyncClient(
        base_url=f"http://127.0.0.1:{proxy_port}",
        limits=httpx.Limits(max_connections=1, max_keepalive_connections=1),
        http2=False,
        trust_env=False,
        timeout=httpx.Timeout(BOUND),
    )
    token = unique("tri")
    observer = httpx.AsyncClient(base_url=args.url, trust_env=False, timeout=httpx.Timeout(BOUND))
    print("warm:", (await client.get("/warm")).status_code)
    print("pool after warm:", pool_info(client))

    extra = {"X-Split-Body": str(args.split_body)} if args.mode == "trigger" else {}

    async def target():
        return await client.post(
            f"/doc/{token}",
            json={"value": "target", "req": f"{token}-target"},
            headers=extra,
        )

    if args.mode == "trigger":
        # Plain C task: production-style cancellation, no pure-Python Task.
        task = asyncio.create_task(target())
        seen = False
        for _ in range(400):
            obs = await client.get("/__health__") if False else None
            view = await observer.get("/__observe__", params={"doc": token})
            if any(row.get("req") == f"{token}-target" for row in view.json()["requests"]):
                seen = True
                break
            await asyncio.sleep(0.002)
        print("trigger seen:", seen, flush=True)
        await asyncio.sleep(args.trigger_sleep)
        task.cancel()
        try:
            await task
            print("target completed before trigger")
        except asyncio.CancelledError:
            print("target cancelled (plain C task)", flush=True)
    else:
        task = TracingCutTask(target(), cut_after=args.k)
        try:
            await task
            print("target completed unexpectedly")
        except asyncio.CancelledError:
            print("target cancelled", flush=True)
    await asyncio.sleep(0)
    print("pool after cancel:", pool_info(client))
    print("proxy writers:", len(proxy._writers), "tasks:", len(proxy._tasks))

    for attempt in (1, 2):
        try:
            resp = await asyncio.wait_for(
                client.post(f"/doc/{token}", json={"value": "survivor", "req": f"{token}-s{attempt}"}),
                BOUND,
            )
            print(f"retry{attempt}: {resp.status_code} {resp.text}")
        except BaseException as exc:
            print(f"retry{attempt}: ERR {type(exc).__name__}: {exc}")
            traceback.print_exc(limit=8)
        print(f"pool after retry{attempt}:", pool_info(client))
        await asyncio.sleep(0.1)

    await client.aclose()
    await observer.aclose()
    await proxy.close()


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
