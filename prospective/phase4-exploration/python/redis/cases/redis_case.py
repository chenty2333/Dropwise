"""redis-py 8.1.0 cancellation cases (round-3 exploration, unfrozen).

Four reuse units, kept separate (never one combined ladder):

    unit tx   session-state operation: WATCH -> MULTI -> SET -> EXEC.
              pool     : real transaction through ``client.pipeline(
                         transaction=True)`` on a ``ConnectionPool(
                         max_connections=1)``; the pipeline context manager
                         performs the library's UNWATCH/release on cancel.
              direct   : one held ``Redis(single_connection_client=True)``
                         connection; the same transaction is issued as four
                         real server commands (WATCH, MULTI, SET, EXEC), with
                         a bounded DISCARD in ``finally`` while MULTI is open.
                         ``client.pipeline()`` is not used here because it
                         checks out a second connection from the pool and
                         would leave the held connection untested.
    unit set  plain full request: one ``SET key target`` round trip; same
              pool / same held connection for the survivor.

Survivor (all units): GET key (acquire-time value), SET key survivor, GET
key (own readback), on the same pool/held connection.  Observer: a separate
direct ``redis.asyncio`` client, never through the pool and never through the
delay proxy.

Pending mechanism: ``DelayProxy`` (retro ``cases/common.py``, 50 ms on every
forwarded chunk, both directions), wrapping only the tested client.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import time

import redis
import redis.asyncio as aioredis

from .common import (  # noqa: E402  (also puts research/python-retro on sys.path)
    BOUND,
    DELAY,
    DelayProxy,
    add_args,
    bound,
    close_quiet,
    compact,
    err_text,
    observation,
    unique_key,
)
from dropwise_py import (  # noqa: E402
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)
from dropwise_py.engine import OpResult, _await_target  # noqa: E402

TARGET_VALUE = "target"
SURVIVOR_VALUE = "survivor"
B3_VALUE = "b3"


class RedisScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.unit = args.unit
        stem = unique_key("dw:r3")
        self.key = f"{stem}:{self.unit}"
        self.b3_key = f"{stem}:b3"
        self.proxy: DelayProxy | None = None
        self.pool = None
        self.client: aioredis.Redis | None = None
        self.observer: aioredis.Redis | None = None
        self.diag: dict = {}

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)

        # observer first: direct to the server, no proxy, no tested pool
        self.observer = aioredis.Redis(
            host=a.host, port=a.port, decode_responses=False,
            socket_connect_timeout=BOUND, socket_timeout=BOUND,
        )
        pong = await bound(self.observer.ping())
        if pong[0] != "ok":
            raise RuntimeError(f"observer connect failed: {pong}")

        if self.mode == "pool":
            self.pool = aioredis.ConnectionPool(
                host=a.host, port=client_port, max_connections=1,
                decode_responses=False, socket_connect_timeout=BOUND,
            )
            self.client = aioredis.Redis(connection_pool=self.pool)
        else:
            self.client = aioredis.Redis(
                host=a.host, port=client_port, single_connection_client=True,
                decode_responses=False, socket_connect_timeout=BOUND,
            )
        warm = await bound(self.client.ping())
        if warm[0] != "ok":
            raise RuntimeError(f"tested client warm-up failed: {warm}")
        self.diag["setup_client_id"] = await bound(self._client_id())
        self.diag["setup_pool"] = self._pool_state()
        # observer-visible baseline of the key, before any target
        self.diag["baseline"] = await bound(self.observer.get(self.key))

    async def _client_id(self):
        return await self.client.client_id()

    def _pool_state(self) -> dict | None:
        if self.pool is None:
            return None
        try:
            return {
                "available": len(self.pool._available_connections),
                "in_use": len(self.pool._in_use_connections),
                "max": self.pool.max_connections,
            }
        except BaseException as exc:
            return {"error": type(exc).__name__}

    def _conn_state(self) -> dict | None:
        if self.mode != "direct" or self.client is None:
            return None
        conn = self.client.connection
        return {
            "connected": bool(conn and conn.is_connected),
            "has_connection": conn is not None,
        }

    # ---- transaction primitives -------------------------------------
    #
    # Adapter contract (round-3 audit): every stage either raises, or the
    # operation returns a structured ``OpResult``.  A dict alone is never the
    # verdict value, and a failed stage can never look like success.  The
    # original exception is re-raised by ``target`` so an uncancelled target
    # failure becomes an engine baseline error instead of a silent pass.
    async def _tx_pipeline(self, key: str, value: str):
        out: dict = {}
        failure = None
        stage = "pipeline"
        try:
            pipe = self.client.pipeline(transaction=True)
            self.diag["stage"] = stage
            async with pipe:
                stage = "watch"
                self.diag["stage"] = stage
                out["watch"] = await pipe.watch(key)
                # optimistic-transaction read inside the WATCH (immediate
                # round trip, not buffered)
                stage = "read"
                self.diag["stage"] = stage
                out["read"] = await pipe.get(key)
                stage = "multi"
                self.diag["stage"] = stage
                pipe.multi()             # client-side MULTI marker
                stage = "queue"
                self.diag["stage"] = stage
                pipe.set(key, value)     # buffered until execute()
                stage = "exec"
                self.diag["stage"] = stage
                out["exec"] = await pipe.execute()  # MULTI + SET + EXEC
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            failure = (stage, exc)
        return out, failure

    async def _tx_explicit(self, key: str, value: str):
        """real MULTI/EXEC issued as commands on the one held connection."""
        out: dict = {}
        in_multi = False
        failure = None
        stage = "watch"
        try:
            self.diag["stage"] = stage
            out["watch"] = await self.client.execute_command("WATCH", key)
            stage = "multi"
            self.diag["stage"] = stage
            out["multi"] = await self.client.execute_command("MULTI")
            in_multi = True
            stage = "queue"
            self.diag["stage"] = stage
            out["queue"] = await self.client.execute_command("SET", key, value)
            stage = "exec"
            self.diag["stage"] = stage
            out["exec"] = await self.client.execute_command("EXEC")
            in_multi = False
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            failure = (stage, exc)
        finally:
            if in_multi:
                # best effort, bounded and recorded: a DISCARD after a
                # committed EXEC is an expected ResponseError, and a hang must
                # not mask the cancellation
                out["discard"] = await bound(
                    self.client.execute_command("DISCARD")
                )
        return out, failure

    async def _tx(self, key: str, value: str):
        if self.mode == "pool":
            return await self._tx_pipeline(key, value)
        return await self._tx_explicit(key, value)

    async def _plain_set(self, key: str, value: str):
        stage = "set"
        self.diag["stage"] = stage
        try:
            return {"set": await self.client.set(key, value)}, None
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {}, (stage, exc)

    async def _unit_op(self, key: str, value: str):
        if self.unit == "tx":
            return await self._tx(key, value)
        return await self._plain_set(key, value)

    @staticmethod
    def _op_result(out: dict, failure, fallback_stage: str) -> OpResult:
        if failure is None:
            return OpResult(ok=True, stage=fallback_stage, value=out)
        stage, exc = failure
        return OpResult(ok=False, stage=stage, value=out, error=err_text(exc))

    # ---- target / B2 / B3 -------------------------------------------
    async def target(self):
        out, failure = await self._unit_op(self.key, TARGET_VALUE)
        if failure is not None:
            stage, exc = failure
            self.diag["target_failure"] = (stage, err_text(exc))
            raise exc
        return out

    async def same(self):
        # B2: same operation, same key, same value.  Structured failure value;
        # CancelledError still propagates.
        out, failure = await self._unit_op(self.key, TARGET_VALUE)
        return self._op_result(out, failure, "same")

    async def different(self):
        # B3: same kind of operation on a different key (tx -> tx; plain unit
        # -> INCR on the b3 key; both are full write requests on the unit).
        if self.unit == "tx":
            out, failure = await self._tx(self.b3_key, B3_VALUE)
            return self._op_result(out, failure, "different")
        stage = "incr"
        self.diag["stage"] = stage
        try:
            out = {"incr": await self.client.incr(self.b3_key)}
            failure = None
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            out, failure = {}, (stage, exc)
        return self._op_result(out, failure, "different")

    # ---- survivor / observer ----------------------------------------
    async def survivor(self):
        out = {"pre_read": await bound(self.client.get(self.key))}
        out["set"] = await bound(self.client.set(self.key, SURVIVOR_VALUE))
        out["own"] = await bound(self.client.get(self.key))
        return out

    async def _tx_state_probe(self):
        """MULTI/PING/EXEC on the same unit, to expose leftover MULTI state.

        Runs after the judged survivor and *after* the independent observer
        read, so it can never change the judged observation.  A connection
        left in MULTI makes the new MULTI fail and PING get queued.
        """
        if self.mode == "pool":
            pipe = self.client.pipeline(transaction=True)
            async with pipe:
                pipe.ping()
                return await pipe.execute()
        out = {}
        await self.client.execute_command("MULTI")
        out["ping"] = await self.client.execute_command("PING")
        out["exec"] = await self.client.execute_command("EXEC")
        return out

    async def observe(self):
        value = await bound(self.observer.get(self.key))
        # diagnostics only, never judged (see _tx_state_probe)
        self.diag["tx_probe"] = await bound(self._tx_state_probe(), 1.0)
        self.diag["survivor_client_id"] = await bound(self._client_id(), 1.0)
        self.diag["survivor_pool"] = self._pool_state()
        self.diag["survivor_conn"] = self._conn_state()
        if self.diag.get("setup_client_id", (None,))[1] != self.diag.get(
            "survivor_client_id", (None,)
        )[1]:
            self.diag["client_id_changed"] = True
        return {"value": value}

    # ---- cleanup -----------------------------------------------------
    async def close(self) -> None:
        print(
            f"DIAG mode={self.mode} unit={self.unit} "
            f"stage={self.diag.get('stage')} "
            f"target_failure={self.diag.get('target_failure')} "
            f"setup_id={self.diag.get('setup_client_id')} "
            f"surv_id={self.diag.get('survivor_client_id')} "
            f"changed={self.diag.get('client_id_changed')} "
            f"pool={self.diag.get('survivor_pool')} "
            f"conn={self.diag.get('survivor_conn')} "
            f"tx_probe={self.diag.get('tx_probe')}",
            flush=True,
        )
        cleanup = {}
        if self.client is not None:
            cleanup["client"] = await close_quiet(self.client.aclose())
        if self.pool is not None:
            cleanup["pool"] = await close_quiet(self.pool.aclose())
        if self.observer is not None:
            cleanup["observer"] = await close_quiet(self.observer.aclose())
        if self.proxy is not None:
            cleanup["proxy"] = await close_quiet(self.proxy.close())
        print(f"DIAG close mode={self.mode} unit={self.unit} {cleanup}", flush=True)

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.key)


# ---------------------------------------------------------------------
# diagnostics: reuse, pre/post, per-yield detail
# ---------------------------------------------------------------------
async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        ids = []
        for _ in range(3):
            ids.append(await bound(sc._client_id()))
        print(f"DIAG reuse client_ids={ids} pool={sc._pool_state()}", flush=True)
        pre = await sc.judged()
        print(f"DIAG pre obs={pre!r}", flush=True)
        tr = await _await_target(sc.target(), None, 12)
        print(
            f"DIAG uncancelled target={tr.outcome}/{tr.yields} result={tr.result!r} "
            f"err={tr.error!r} diag={sc.diag!r}",
            flush=True,
        )
        post = await sc.judged()
        print(f"DIAG post obs={post!r}", flush=True)
        print(f"DIAG pre==post {pre == post}", flush=True)
    finally:
        await close_quiet(sc.close())


async def probe_run(factory, ks: list[int]) -> None:
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
                f"yields={tr.yields} err={tr.error!r} result={tr.result!r}\n"
                f"      diag={sc.diag!r}\n"
                f"      survivor={surv!r}\n"
                f"      observer={obs!r} seconds={time.monotonic() - started:.2f}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


async def recover_run(factory, plans: list[int]) -> None:
    """Recovery contrast after a cancelled target (round-3 requirement 4).

    Issues explicit recovery commands on the same unit and tags the next write
    with a unique token, so a stale/queued reply from the cancelled target is
    distinguishable from a new reply.  A clean direct/pool unit answers:
    DISCARD -> ResponseError (no MULTI open), UNWATCH -> True, SET/GET token ->
    the token, PING -> True.  A leftover MULTI would answer DISCARD -> True
    (recovering) and the tagged read would show the old/queued value.
    """
    for k in plans:
        sc = factory()
        await asyncio.wait_for(sc.setup(), 12)
        try:
            tr = await _await_target(sc.target(), k, 12)
            token = f"recover-{k}"
            rec = {}
            rec["discard"] = await bound(sc.client.execute_command("DISCARD"))
            rec["unwatch"] = await bound(sc.client.execute_command("UNWATCH"))
            rec["set_new"] = await bound(sc.client.set(sc.key, token))
            rec["get_new"] = await bound(sc.client.get(sc.key))
            rec["ping"] = await bound(sc.client.ping())
            rec["client_id"] = await bound(sc.client.client_id())
            obs = await bound(sc.observer.get(sc.key))
            print(
                f"RECOVER k={k} target={tr.outcome}/y{tr.yields} "
                f"realized={tr.realized} err={tr.error!r} "
                f"rec={rec!r} observer_new={obs!r} diag={sc.diag!r}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


# ---------------------------------------------------------------------
def build_factory(args):
    return lambda: RedisScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_args(parser)
    parser.add_argument("--diag", action="store_true", help="reuse + pre/post details")
    parser.add_argument(
        "--recover", default=None,
        help="comma-separated yields: recovery contrast after cancelling there",
    )
    args = parser.parse_args()

    import sys

    print(
        f"case=redis-py mode={args.mode} unit={args.unit} redis={redis.__version__} "
        f"python={sys.version.split()[0]} host={args.host} port={args.port} "
        f"round={args.round} delay={DELAY} bound={BOUND}",
        flush=True,
    )
    factory = build_factory(args)

    if args.probe:
        ks = [int(x) for x in args.probe.split(",") if x.strip()]
        await probe_run(factory, ks)
        return
    if args.diag:
        await diag_run(factory)
        return
    if args.recover:
        plans = [int(x) for x in args.recover.split(",") if x.strip()]
        await recover_run(factory, plans)
        return

    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    timeout = 12.0
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1("redis", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "redis", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "redis", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed("redis", factory, reps=args.reps, timeout=timeout)
        else:
            raise SystemExit(f"unknown level {level}")
        print_report(r)
        print(compact(r), flush=True)
        reports.append(r)

    print("=== FINAL ===", flush=True)
    for r in reports:
        print(f"unit={args.unit} mode={args.mode} {compact(r)}", flush=True)
        print(f"violations_by_plan={[t.plan for t in r.violations()]}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
