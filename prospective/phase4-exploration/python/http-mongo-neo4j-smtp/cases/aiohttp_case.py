"""aiohttp 3.14.5: one POST that sets a server-side document, then cancellation.

Target:    POST /doc/<token> {"value": "target", "req": "<token>-target"}
           through the tested ClientSession, one TCPConnector with
           limit=1, limit_per_host=1, force_close=False, HTTP/1.1 only.
Survivor:  POST the same document with value "survivor" and its own request id,
           then GET the same document on the same session (same-connection
           readback).  The compared observation keeps only invariant facts:
           whether the server log saw the target/survivor request, the final
           document value, and whether the survivor's own request id came back.
Observer:  a second ClientSession, direct to the server (not through the delay
           proxy), reads the server-side document and per-connection request
           log via /__observe__.

Pending:   the read-only DelayProxy from research/python-retro in front of the
           server, delaying every forwarded chunk by 50 ms (both directions).
Bound:     2 s per operation; a timeout becomes an "ERR ..." string.

B3 uses a GET of the same document (a different operation on the same object,
not the same POST with another value).  The target document, its request log
and the connector state are the same objects in every level.
"""

from __future__ import annotations

import asyncio

import aiohttp

from .support import (
    BOUND,
    ENGINE_TIMEOUT,
    DelayProxy,
    Expect,
    Scenario,
    base_parser,
    bounded,
    compact,
    explore_observed,
    level_b1,
    level_retry,
    print_full_report,
    unique,
)

TARGET_VALUE = "target"
SURVIVOR_VALUE = "survivor"


def _resp(resp):
    """Concrete (status, body) or the bounded error string."""
    if isinstance(resp, str):
        return resp
    return {"status": resp[0], "body": resp[1]}


class AiohttpScenario(Scenario):
    def __init__(self, server_url: str) -> None:
        self.server_url = server_url
        self.base = ""
        self.token = ""
        self.doc = ""
        self.proxy: DelayProxy | None = None
        self.session: aiohttp.ClientSession | None = None
        self.observer: aiohttp.ClientSession | None = None

    # ---- setup / close -------------------------------------------------

    async def setup(self) -> None:
        from urllib.parse import urlparse

        self.token = unique("ah")
        self.doc = self.token
        self.target_req = f"{self.token}-target"
        self.survivor_req = f"{self.token}-survivor"
        self.read_req = f"{self.token}-read"
        self.diff_req = f"{self.token}-b3"

        parsed = urlparse(self.server_url)
        host, port = parsed.hostname or "127.0.0.1", parsed.port or 80
        self.proxy = DelayProxy(host, port, delay=0.05)
        proxy_port = await self.proxy.start()
        self.base = f"http://127.0.0.1:{proxy_port}"

        connector = aiohttp.TCPConnector(
            limit=1, limit_per_host=1, force_close=False, enable_cleanup_closed=False
        )
        self.session = aiohttp.ClientSession(
            connector=connector,
            timeout=aiohttp.ClientTimeout(total=BOUND),
            trust_env=False,
            auto_decompress=True,
        )
        # The observer is a separate session and does not go through the proxy.
        self.observer = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=BOUND), trust_env=False
        )
        warm = await self._probe(self.session, self.base + "/warm", "warmup")
        health = await self._probe(
            self.observer, self.server_url + "/__health__", "observer-health"
        )
        print(
            f"DIAG setup token={self.token} proxy_port={proxy_port} "
            f"warm={warm!r} health={health!r} "
            f"connector=limit1/per_host1/force_close=False",
            flush=True,
        )
        if not (isinstance(warm, dict) and warm.get("status") == 200):
            raise RuntimeError(f"warm-up failed: {warm!r}")
        if not (isinstance(health, dict) and health.get("status") == 200):
            raise RuntimeError(f"observer health failed: {health!r}")

    async def close(self) -> None:
        for session in (self.session, self.observer):
            if session is not None:
                try:
                    await asyncio.wait_for(session.close(), BOUND)
                except BaseException:  # noqa: BLE001 - cleanup only
                    pass
        if self.proxy is not None:
            try:
                await asyncio.wait_for(self.proxy.close(), BOUND)
            except BaseException:  # noqa: BLE001 - cleanup only
                pass

    # ---- operations ----------------------------------------------------

    async def _probe(self, session, url: str, label: str):
        """Bounded GET of a diagnostics endpoint, returned as (status, body)."""

        async def op():
            async with session.get(url) as resp:
                return resp.status, await resp.json(content_type=None)

        return _resp(await bounded(op(), label))

    async def _post(self, value: str, req: str):
        assert self.session is not None

        async def op():
            async with self.session.post(
                f"{self.base}/doc/{self.doc}", json={"value": value, "req": req}
            ) as resp:
                body = await resp.json(content_type=None)
            return resp.status, body

        return _resp(await bounded(op(), f"post-{value}"))

    async def _get(self, req: str, *, through: str = "tested"):
        assert self.session is not None and self.observer is not None
        session = self.session if through == "tested" else self.observer
        url = (self.base if through == "tested" else self.server_url) + f"/doc/{self.doc}"

        async def op():
            async with session.get(url, params={"req": req}) as resp:
                body = await resp.json(content_type=None)
            return resp.status, body

        return _resp(await bounded(op(), f"get-{req}"))

    async def target(self):
        result = await self._post(TARGET_VALUE, self.target_req)
        print(f"DIAG target token={self.token} result={result!r}", flush=True)
        return result

    async def same(self):
        return await self._post(TARGET_VALUE, self.target_req)

    async def different(self):
        # Different operation of the same kind: read the same document.
        return await self._get(self.diff_req)

    def _pool_diag(self) -> str:
        if self.session is None:
            return "session=None"
        try:
            connector = self.session.connector
            loop = self.session._loop  # noqa: SLF001 - diagnostic text
            now = loop.time()
            pooled = {}
            for key, dq in connector._conns.items():  # noqa: SLF001
                pooled[str(key)] = [
                    {
                        "connected": proto.is_connected(),
                        "should_close": getattr(proto, "should_close", None),
                        "idle_s": round(now - ts, 3),
                    }
                    for proto, ts in dq
                ]
            total = None
            if hasattr(connector, "total_connections"):
                total = connector.total_connections()
            return (
                f"total={total} acquired={len(connector._acquired)} "  # noqa: SLF001
                f"waiters={len(connector._waiters)} pooled={pooled}"  # noqa: SLF001
            )
        except Exception as exc:  # noqa: BLE001 - diagnostic only
            return f"pool-diag-error {type(exc).__name__}: {exc}"

    async def survivor(self):
        print(f"DIAG pre-survivor pool token={self.token} {self._pool_diag()}", flush=True)
        post = await self._post(SURVIVOR_VALUE, self.survivor_req)
        read = await self._get(self.read_req)
        obs_view = await self._probe(
            self.observer,
            f"{self.server_url}/__observe__?doc={self.doc}",
            "observe",
        )
        raw = obs_view
        print(
            f"DIAG survivor token={self.token} post={post!r} read={read!r} "
            f"pool_after={self._pool_diag()}",
            flush=True,
        )
        print(f"DIAG observer token={self.token} view={raw!r}", flush=True)

        # Compared observation: only invariant facts.  Connection ids, the
        # observer call itself and timings stay in the DIAG lines; a legal
        # reconnect must not be turned into a violation by the oracle.
        log = raw.get("body", {}).get("requests", []) if isinstance(raw, dict) else []
        seen_target = any(row.get("req") == self.target_req for row in log)
        seen_survivor = any(row.get("req") == self.survivor_req for row in log)
        post_ok = (
            isinstance(post, dict)
            and post.get("status") == 200
            and post.get("body", {}).get("req") == self.survivor_req
            and post.get("body", {}).get("stored") == SURVIVOR_VALUE
        )
        read_ok = (
            isinstance(read, dict)
            and read.get("status") == 200
            and read.get("body", {}).get("req") == self.read_req
            and read.get("body", {}).get("value") == SURVIVOR_VALUE
        )
        observer_doc = raw.get("body", {}).get("doc") if isinstance(raw, dict) else raw
        return {
            "target_logged": seen_target,
            "survivor_logged": seen_survivor,
            "doc_after": observer_doc,
            "survivor_post_ok": post_ok,
            "readback_ok": read_ok,
        }

    async def observe(self):
        resp = await self._probe(
            self.observer,
            f"{self.server_url}/__observe__?doc={self.doc}",
            "observe",
        )
        body = resp.get("body", {}) if isinstance(resp, dict) else {}
        return {"doc": body.get("doc"), "requests": len(body.get("requests", []))}


async def run_levels(args, levels, plans) -> int:
    factory = lambda: AiohttpScenario(args.url)

    async def one(level: str):
        if level == "B1":
            return await level_b1(
                "aiohttp", factory, reps=args.reps, timeout=ENGINE_TIMEOUT, plans=plans
            )
        if level in ("B2", "B3"):
            return await level_retry(
                "aiohttp",
                factory,
                level=level,
                retry="same" if level == "B2" else "different",
                reps=args.reps,
                timeout=ENGINE_TIMEOUT,
                plans=plans,
            )
        if level == "M1":
            return await explore_observed(
                "aiohttp",
                factory,
                expect=Expect.ATOMIC,
                reps=args.reps,
                timeout=ENGINE_TIMEOUT,
                plans=plans,
            )
        raise SystemExit(f"unknown level {level}")

    reports = []
    for level in levels:
        print(f"\n=== LEVEL {level} ===", flush=True)
        report = await one(level)
        print_full_report(report)
        print(compact(report), flush=True)
        reports.append(report)
    print("\n=== FINAL ===", flush=True)
    for report in reports:
        print(compact(report), flush=True)
        print(
            f"violations_by_plan={[(t.plan, t.rep) for t in report.violations()]}",
            flush=True,
        )
        print(
            f"unrealized_by_plan={[(t.plan, t.rep) for t in report.unrealized()]}",
            flush=True,
        )
    return 0


async def main() -> None:
    parser = base_parser(__doc__)
    parser.add_argument(
        "--plans",
        default=None,
        help="optional comma-separated yield indices (triage subset)",
    )
    args = parser.parse_args()
    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    plans = [int(x) for x in args.plans.split(",")] if args.plans else None
    print(
        f"case=aiohttp version={aiohttp.__version__} url={args.url} "
        f"reps={args.reps} levels={levels} plans={plans}",
        flush=True,
    )
    await run_levels(args, levels, plans)


if __name__ == "__main__":
    import contextlib

    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
