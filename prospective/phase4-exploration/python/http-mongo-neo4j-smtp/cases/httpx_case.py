"""httpx 0.28.1: one POST that sets a server-side document, then cancellation.

Target:    POST /doc/<token> {"value": "target", "req": "<token>-target"}
           through the tested AsyncClient (Limits(max_connections=1,
           max_keepalive_connections=1), http2=False, HTTP/1.1 only).
Survivor:  POST the same document with value "survivor" and its own request id,
           then GET the same document on the same client (same-connection
           readback).  The compared observation keeps only invariant facts:
           whether the server log saw the target/survivor request, the final
           document value, and whether the survivor's own request id came back.
Observer:  a second AsyncClient, direct to the server (not through the delay
           proxy), reads the server-side document and per-connection request
           log via /__observe__.

Pending:   the read-only DelayProxy from research/python-retro in front of the
           server, delaying every forwarded chunk by 50 ms (both directions).
Bound:     2 s per operation; a timeout becomes an "ERR ..." string.
"""

from __future__ import annotations

import asyncio
import json
import sys

import httpx

from .support import (
    BOUND,
    ENGINE_TIMEOUT,
    Expect,
    DelayProxy,
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
DIFFERENT_VALUE = "b3"


def _resp_json(resp):
    """Concrete (status, body) or the bounded error string."""
    if isinstance(resp, str):
        return resp
    try:
        body = resp.json()
    except Exception as exc:  # noqa: BLE001 - recorded as the value
        body = f"ERR body {type(exc).__name__}: {exc}"
    return {"status": resp.status_code, "body": body}


class HttpxScenario(Scenario):
    def __init__(self, server_url: str) -> None:
        self.server_url = server_url
        self.token = ""
        self.doc = ""
        self.proxy: DelayProxy | None = None
        self.client: httpx.AsyncClient | None = None
        self.observer: httpx.AsyncClient | None = None

    # ---- setup / close -------------------------------------------------

    async def setup(self) -> None:
        self.token = unique("hx")
        self.doc = self.token
        self.target_req = f"{self.token}-target"
        self.survivor_req = f"{self.token}-survivor"
        self.diff_req = f"{self.token}-b3"
        self.read_req = f"{self.token}-read"
        from urllib.parse import urlparse

        parsed = urlparse(self.server_url)
        host, port = parsed.hostname or "127.0.0.1", parsed.port or 80
        self.proxy = DelayProxy(host, port, delay=0.05)
        proxy_port = await self.proxy.start()
        self.client = httpx.AsyncClient(
            base_url=f"http://127.0.0.1:{proxy_port}",
            limits=httpx.Limits(max_connections=1, max_keepalive_connections=1),
            http2=False,
            trust_env=False,
            timeout=httpx.Timeout(BOUND),
        )
        # The observer is a separate client and does not go through the proxy.
        self.observer = httpx.AsyncClient(
            base_url=self.server_url, trust_env=False, timeout=httpx.Timeout(BOUND)
        )
        warm = await bounded(self.client.get("/warm"), "warmup")
        health = await bounded(self.observer.get("/__health__"), "observer-health")
        print(
            f"DIAG setup token={self.token} proxy_port={proxy_port} "
            f"warm={_resp_json(warm)!r} health={_resp_json(health)!r}",
            flush=True,
        )

    async def close(self) -> None:
        for client in (self.client, self.observer):
            if client is not None:
                try:
                    await asyncio.wait_for(client.aclose(), BOUND)
                except BaseException:  # noqa: BLE001 - cleanup only
                    pass
        if self.proxy is not None:
            try:
                await asyncio.wait_for(self.proxy.close(), BOUND)
            except BaseException:  # noqa: BLE001 - cleanup only
                pass

    # ---- operations ----------------------------------------------------

    async def _post(self, value: str, req: str):
        assert self.client is not None
        resp = await bounded(
            self.client.post(
                f"/doc/{self.doc}", json={"value": value, "req": req}
            ),
            f"post-{value}",
        )
        return _resp_json(resp)

    async def target(self):
        result = await self._post(TARGET_VALUE, self.target_req)
        print(f"DIAG target token={self.token} result={result!r}", flush=True)
        return result

    async def same(self):
        return await self._post(TARGET_VALUE, self.target_req)

    async def different(self):
        return await self._post(DIFFERENT_VALUE, self.diff_req)

    def _pool_diag(self) -> str:
        if self.client is None:
            return "client=None"
        try:
            pool = self.client._transport._pool  # noqa: SLF001 - diagnostic text
            conns = [conn.info() for conn in pool.connections]
            return f"conns={conns} queued={len(pool._requests)}"
        except Exception as exc:  # noqa: BLE001 - diagnostic only
            return f"pool-diag-error {type(exc).__name__}: {exc}"

    async def survivor(self):
        print(f"DIAG pre-survivor pool token={self.token} {self._pool_diag()}", flush=True)
        post = await self._post(SURVIVOR_VALUE, self.survivor_req)
        read = await bounded(
            self.client.get(f"/doc/{self.doc}", params={"req": self.read_req}),
            "readback",
        )
        read = _resp_json(read)
        obs_view = await bounded(
            self.observer.get("/__observe__", params={"doc": self.doc}), "observe"
        )
        raw = _resp_json(obs_view)
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
        observer_doc = (
            raw.get("body", {}).get("doc") if isinstance(raw, dict) else raw
        )
        return {
            "target_logged": seen_target,
            "survivor_logged": seen_survivor,
            "doc_after": observer_doc,
            "survivor_post_ok": post_ok,
            "readback_ok": read_ok,
        }

    async def observe(self):
        resp = await bounded(
            self.observer.get("/__observe__", params={"doc": self.doc}), "observe"
        )
        raw = _resp_json(resp)
        body = raw.get("body", {}) if isinstance(raw, dict) else {}
        return {"doc": body.get("doc"), "requests": len(body.get("requests", []))}


async def run_levels(args, levels, plans) -> int:
    factory = lambda: HttpxScenario(args.url)

    async def one(level: str):
        if level == "B1":
            report = await level_b1(
                "httpx", factory, reps=args.reps, timeout=ENGINE_TIMEOUT, plans=plans
            )
        elif level in ("B2", "B3"):
            report = await level_retry(
                "httpx",
                factory,
                level=level,
                retry="same" if level == "B2" else "different",
                reps=args.reps,
                timeout=ENGINE_TIMEOUT,
                plans=plans,
            )
        elif level == "M1":
            report = await explore_observed(
                "httpx",
                factory,
                expect=Expect.ATOMIC,
                reps=args.reps,
                timeout=ENGINE_TIMEOUT,
                plans=plans,
            )
        else:
            raise SystemExit(f"unknown level {level}")
        return report

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
        f"case=httpx version={httpx.__version__} httpcore="
        f"{__import__('httpcore').__version__} url={args.url} "
        f"reps={args.reps} levels={levels}",
        flush=True,
    )
    await run_levels(args, levels, plans)


if __name__ == "__main__":
    import contextlib

    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
