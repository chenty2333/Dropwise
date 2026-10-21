"""curl-cffi 0.16.3: one POST that sets a server-side document, then cancellation.

Target:    POST /doc/<token> {"value": "target", "req": "<token>-target"}
           through the tested ``curl_cffi.requests.AsyncSession`` with
           ``max_clients=1`` and HTTP/1.1 forced (``CurlHttpVersion.V1_1``).
Survivor:  POST the same document with value "survivor" and its own request id,
           then GET the same document on the same session (same-connection
           readback).  The compared observation keeps only invariant facts:
           whether the server log saw the target/survivor request, the final
           document value, and whether the survivor's own request id came back.
Observer:  a second AsyncSession, direct to the server (not through the delay
           proxy), reads the server-side document and per-connection request
           log via /__observe__.

Pending:   the read-only DelayProxy from research/python-retro in front of the
           server, delaying every forwarded chunk by 50 ms (both directions).
Bound:     2 s per operation; a timeout becomes an "ERR ..." string.

Coverage limit (native callbacks): the engine's yield ladder can only see the
Python-level suspensions of the target coroutine.  libcurl performs the
transfer in C inside the AsyncCurl socket callbacks, so body reads, TLS/HTTP
state transitions and the completion callback are *not* enumerable as awaits.
A low yield count means the realised cuts cover only the outer ``await`` of the
request, not every protocol state; this is recorded as a coverage gap in the
report, not repaired by inventing cut points.
"""

from __future__ import annotations

import asyncio

import curl_cffi
from curl_cffi.const import CurlHttpVersion
from curl_cffi.requests import AsyncSession

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


class CurlCffiScenario(Scenario):
    def __init__(self, server_url: str) -> None:
        self.server_url = server_url
        self.base = ""
        self.token = ""
        self.doc = ""
        self.proxy: DelayProxy | None = None
        self.session: AsyncSession | None = None
        self.observer: AsyncSession | None = None

    # ---- setup / close -------------------------------------------------

    async def setup(self) -> None:
        from urllib.parse import urlparse

        self.token = unique("cc")
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

        self.session = AsyncSession(
            max_clients=1,
            timeout=BOUND,
            trust_env=False,
            http_version=CurlHttpVersion.V1_1,
        )
        # The observer is a separate session and does not go through the proxy.
        self.observer = AsyncSession(
            max_clients=1,
            timeout=BOUND,
            trust_env=False,
            http_version=CurlHttpVersion.V1_1,
        )
        warm = await self._probe(self.session, self.base + "/warm", "warmup")
        health = await self._probe(
            self.observer, self.server_url + "/__health__", "observer-health"
        )
        print(
            f"DIAG setup token={self.token} proxy_port={proxy_port} "
            f"warm={warm!r} health={health!r} "
            f"curl_cffi={curl_cffi.__version__} curl={curl_cffi.__curl_version__} "
            f"max_clients=1 http_version=V1_1",
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
            resp = await session.get(url)
            return resp.status_code, resp.json()

        return _resp(await bounded(op(), label))

    async def _post(self, value: str, req: str):
        assert self.session is not None

        async def op():
            resp = await self.session.post(
                f"{self.base}/doc/{self.doc}", json={"value": value, "req": req}
            )
            return resp.status_code, resp.json()

        return _resp(await bounded(op(), f"post-{value}"))

    async def _get(self, req: str, *, through: str = "tested"):
        assert self.session is not None and self.observer is not None
        session = self.session if through == "tested" else self.observer
        url = (self.base if through == "tested" else self.server_url) + f"/doc/{self.doc}"

        async def op():
            resp = await session.get(url, params={"req": req})
            return resp.status_code, resp.json()

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
            qsize = self.session.pool.qsize()
            inflight = None
            acurl = getattr(self.session, "_acurl", None)
            if acurl is not None:
                inflight = len(acurl._curl2future)  # noqa: SLF001
            return f"pool_qsize={qsize} inflight={inflight}"
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
    factory = lambda: CurlCffiScenario(args.url)

    async def one(level: str):
        if level == "B1":
            return await level_b1(
                "curl-cffi", factory, reps=args.reps, timeout=ENGINE_TIMEOUT, plans=plans
            )
        if level in ("B2", "B3"):
            return await level_retry(
                "curl-cffi",
                factory,
                level=level,
                retry="same" if level == "B2" else "different",
                reps=args.reps,
                timeout=ENGINE_TIMEOUT,
                plans=plans,
            )
        if level == "M1":
            return await explore_observed(
                "curl-cffi",
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
        f"case=curl-cffi version={curl_cffi.__version__} "
        f"curl={curl_cffi.__curl_version__} url={args.url} "
        f"reps={args.reps} levels={levels} plans={plans}",
        flush=True,
    )
    await run_levels(args, levels, plans)


if __name__ == "__main__":
    import contextlib

    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
