"""neo4j 6.4.1 AsyncGraphDatabase pool cases (max_connection_pool_size=1).

Two reuse units on the same skeleton:

  txn    target = `session.begin_transaction()` + `tx.run(MERGE target)` +
         `tx.commit()` — the explicit-transaction path.
  plain  target = one ordinary `session.run(MERGE target)` with the result
         fully consumed (`single()` + `consume()`).

Survivor = the same AsyncDriver (pool size 1), MERGEs the survivor node in the
*same* label and reads it back in the same session.  Observer = an independent
AsyncDriver, direct to the server (never through the proxy, never through the
tested driver), reads every node of the label.

Pending mechanism: `DelayProxy` (retro `cases/common.py`, 50 ms per forwarded
server->client chunk) wrapping only the tested driver.

Target cleanup is the documented explicit form: roll back an open transaction,
then close the session.  The transaction/session state *before* cleanup is
recorded in `diag`; the judged observation only contains the pool state at
survivor acquire, the survivor's concrete write/readback, and the independent
observer's nodes.  Whether the pooled connection was reused or discarded is
recorded from the driver's own pool counters and connection identities.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import time

from neo4j import AsyncGraphDatabase

from .common import (  # noqa: E402  (also puts research/python-retro on sys.path)
    BOUND,
    DELAY,
    DelayProxy,
    add_service_args,
    bound,
    close_quiet,
    compact,
    err_text,
    observation,
    op_from,
    unique_name,
)
from dropwise_py import (  # noqa: E402
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)
from dropwise_py.engine import OpResult, _await_target  # noqa: E402

TARGET_KEY = "target"
SURVIVOR_KEY = "survivor"
B3_KEY = "b3"


class Neo4jScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.label = unique_name("DwR3")
        self.proxy: DelayProxy | None = None
        self.driver = None
        self.observer = None
        self.session = None
        self.tx = None
        self.diag: dict = {}

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        uri = f"bolt://127.0.0.1:{client_port}"
        direct = f"bolt://{a.host}:{a.port}"
        auth = (a.user, a.password)

        obs = AsyncGraphDatabase.driver(
            direct, auth=auth, max_connection_pool_size=2,
            connection_timeout=BOUND,
        )
        ping = await bound(obs.verify_connectivity())
        if ping[0] != "ok":
            raise RuntimeError(f"observer connect failed: {ping}")
        self.observer = obs

        self.driver = AsyncGraphDatabase.driver(
            uri, auth=auth, max_connection_pool_size=1,
            connection_timeout=BOUND,
        )
        warm = await bound(self.driver.verify_connectivity())
        if warm[0] != "ok":
            raise RuntimeError(f"tested driver connect failed: {warm}")
        w = await bound(self._scalar("RETURN 1 AS x"))
        if w[0] != "ok":
            raise RuntimeError(f"warm-up failed: {w}")
        self.diag["setup_pool"] = self._pool()
        self.diag["setup_ids"] = self._pool_ids()

    # ---- state probes -------------------------------------------------
    def _addr(self):
        try:
            return self.driver._pool.address
        except BaseException:
            return None

    def _pool(self) -> dict:
        try:
            p = self.driver._pool
            addr = p.address
            conns = p.connections.get(addr, ())
            return {
                "in_use": p.in_use_connection_count(addr),
                "idle": len(list(conns)),
                "max": p.pool_config.max_connection_pool_size,
            }
        except BaseException as exc:
            return {"error": err_text(exc)}

    def _pool_ids(self) -> dict:
        try:
            p = self.driver._pool
            addr = p.address
            return {"ids": [id(c) for c in list(p.connections.get(addr, ()))],
                    "addr": str(addr)}
        except BaseException as exc:
            return {"error": err_text(exc)}

    def _tx_state(self):
        tx = self.tx
        if tx is None:
            return None
        try:
            closed = bool(tx.closed())
        except BaseException:
            closed = None
        return {"closed": closed}

    async def _rollback_if_open(self, tx):
        if tx is None:
            return "no_tx"
        try:
            closed = tx.closed()
        except BaseException as exc:
            return f"state_err {err_text(exc)}"
        if closed:
            return "already_closed"
        return await bound(tx.rollback())

    # ---- helpers ------------------------------------------------------
    def _q(self, clause: str) -> str:
        return f"{clause}"

    async def _scalar(self, query: str):
        session = self.driver.session()
        try:
            res = await bound(session.run(query))
            if res[0] != "ok":
                return res
            rec = await bound(res[1].single())
            if rec[0] != "ok":
                return rec
            return ("ok", None if rec[1] is None else rec[1].data())
        finally:
            await bound(session.close())

    def _session_write(self, key: str, value: int):
        q = (
            f"MERGE (n:`{self.label}` {{k: $k}}) "
            "SET n.v = $v RETURN n.k AS k"
        )
        return q, {"k": key, "v": value}

    async def _session_run(self, query: str, params: dict) -> OpResult:
        """One ordinary session.run + full consumption; structured result."""
        out: dict = {}
        session = self.driver.session()
        self.session = session
        run = ("err", "not started")
        rec = ("err", "not started")
        cons = ("err", "not started")
        try:
            run = await bound(session.run(query, **params))
            out["run"] = "ok" if run[0] == "ok" else run
            if run[0] == "ok":
                rec = await bound(run[1].single())
                out["record"] = (
                    ("ok", None if rec[1] is None else rec[1].data())
                    if rec[0] == "ok" else rec
                )
                cons = await bound(run[1].consume())
                out["consume"] = ("ok", True) if cons[0] == "ok" else cons
        except BaseException as exc:
            out["escaped"] = err_text(exc)
            raise
        finally:
            self.diag["target_final"] = {
                "tx": self._tx_state(), "pool": self._pool(),
            }
            close = await bound(session.close())
            self.diag["session_close"] = close
            self.diag["after_target"] = {
                "pool": self._pool(), "ids": self._pool_ids(),
            }
            out["cleanup"] = {"session_close": close}
        return op_from(
            "plain", {"run": run, "record": rec, "consume": cons}, out
        )

    # ---- target ------------------------------------------------------
    async def _txn(self, key: str, value: int) -> OpResult:
        out: dict = {}
        session = self.driver.session()
        self.session = session
        tx = None
        begin = ("err", "not started")
        run = ("err", "not started")
        rec = ("err", "not started")
        commit = ("err", "not started")
        try:
            begin = await bound(session.begin_transaction())
            out["begin"] = "ok" if begin[0] == "ok" else begin
            if begin[0] == "ok":
                tx = begin[1]
                self.tx = tx
                out["tx_after_begin"] = self._tx_state()
                q, params = self._session_write(key, value)
                run = await bound(tx.run(q, **params))
                out["run"] = "ok" if run[0] == "ok" else run
                if run[0] == "ok":
                    rec = await bound(run[1].single())
                    out["record"] = (
                        ("ok", None if rec[1] is None else rec[1].data())
                        if rec[0] == "ok" else rec
                    )
                out["tx_after_run"] = self._tx_state()
                commit = await bound(tx.commit())
                out["commit"] = "ok" if commit[0] == "ok" else commit
                out["commit_bookmark"] = (
                    None if commit[0] != "ok" else str(commit[1])[:12]
                )
                out["tx_after_commit"] = self._tx_state()
        except BaseException as exc:
            out["escaped"] = err_text(exc)
            raise
        finally:
            self.diag["target_final"] = {
                "tx": self._tx_state(), "pool": self._pool(),
            }
            rollback = await self._rollback_if_open(tx)
            close = await bound(session.close())
            self.diag["rollback"] = rollback
            self.diag["session_close"] = close
            self.diag["after_target"] = {
                "pool": self._pool(), "ids": self._pool_ids(),
            }
            out["cleanup"] = {"rollback": rollback, "session_close": close}
        return op_from(
            "txn",
            {"begin": begin, "run": run, "record": rec, "commit": commit},
            out,
        )

    async def _plain(self, key: str, value: int) -> OpResult:
        q, params = self._session_write(key, value)
        return await self._session_run(q, params)

    async def _create_b3(self) -> OpResult:
        # a different write operation (CREATE, not MERGE) for B3
        q = f"CREATE (n:`{self.label}` {{k: $k, v: $v}}) RETURN n.k AS k"
        return await self._session_run(q, {"k": B3_KEY, "v": 3})

    async def target(self):
        op = (await self._txn(TARGET_KEY, 1) if self.mode == "txn"
              else await self._plain(TARGET_KEY, 1))
        if not op.ok:
            # An uncancelled target must never "complete" with a failed stage.
            raise RuntimeError(f"target failed at {op.stage}: {op.error}")
        return op

    async def same(self):
        return (await self._txn(TARGET_KEY, 1) if self.mode == "txn"
                else await self._plain(TARGET_KEY, 1))

    async def different(self):
        return (await self._plain(B3_KEY, 3) if self.mode == "txn"
                else await self._create_b3())

    # ---- survivor / observer ----------------------------------------
    async def _one(self, session, query: str, params: dict | None = None):
        res = await bound(session.run(query, **(params or {})))
        if res[0] != "ok":
            return res
        rec = await bound(res[1].single())
        consumed = await bound(res[1].consume())
        if rec[0] != "ok":
            return rec
        if consumed[0] != "ok":
            return consumed
        return ("ok", None if rec[1] is None else rec[1].data())

    async def survivor(self):
        # acquire-time pool state is diagnostic only; a leaked sole connection
        # surfaces as a bounded survivor timeout instead.
        out = {}
        self.diag["survivor_start"] = {
            "pool": self._pool(),
            "ids": self._pool_ids(),
            "tx": self._tx_state(),
            "session_closed": getattr(self.session, "_closed", None),
        }
        session = self.driver.session()
        write = ("err", "not started")
        own = ("err", "not started")
        try:
            q, params = self._session_write(SURVIVOR_KEY, 7)
            write = await self._one(session, q, params)
            out["write"] = write
            if write[0] == "ok":
                own = await self._one(
                    session,
                    f"MATCH (n:`{self.label}` {{k: $k}}) RETURN n.v AS v",
                    {"k": SURVIVOR_KEY},
                )
            out["own"] = own
        finally:
            await bound(session.close())
        return op_from("survivor", {"write": write, "own": own}, out)

    async def observe(self):
        session = self.observer.session()
        try:
            res = await bound(session.run(
                f"MATCH (n:`{self.label}`) RETURN n.k AS k, n.v AS v ORDER BY k"
            ))
            if res[0] != "ok":
                return OpResult(False, "observer", value={"nodes": None},
                                error=f"{res[0]}: {res[1]}")
            recs = await bound(res[1].data())
            if recs[0] != "ok":
                return OpResult(False, "observer", value={"nodes": None},
                                error=f"{recs[0]}: {recs[1]}")
            return OpResult(True, "observer",
                            value={"nodes": [[r["k"], r["v"]] for r in recs[1]]})
        finally:
            await bound(session.close())

    # ---- cleanup -----------------------------------------------------
    async def close(self) -> None:
        await close_quiet(self._rollback_if_open(self.tx))
        if self.session is not None:
            await close_quiet(self.session.close())
        if self.driver is not None:
            await close_quiet(self.driver.close())
        if self.observer is not None:
            await close_quiet(self.observer.close())
        if self.proxy is not None:
            await close_quiet(self.proxy.close())

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.label)


# ---------------------------------------------------------------------
# diagnostics: reuse, pre/post, per-yield detail
# ---------------------------------------------------------------------
async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        print(f"DIAG setup pool={sc.diag.get('setup_pool')} "
              f"ids={sc.diag.get('setup_ids')}", flush=True)
        states = []
        for _ in range(3):
            await bound(sc._scalar("RETURN 1 AS x"))
            states.append(sc._pool_ids())
        print(f"DIAG reuse after 3 pings={states}", flush=True)

        pre = await sc.judged()
        print(f"DIAG pre={pre!r}", flush=True)
        tr = await _await_target(sc.target(), None, 12)
        print(
            f"DIAG post target={tr.outcome}/{tr.yields} result={tr.result!r} "
            f"diag={sc.diag!r}",
            flush=True,
        )
        post = await sc.judged()
        print(f"DIAG post={post!r}", flush=True)
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
                f"yields={tr.yields} err={tr.error!r} diag={sc.diag!r}\n"
                f"        survivor={surv!r}\n"
                f"        observer={obs!r} seconds={time.monotonic() - started:.2f}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


# ---------------------------------------------------------------------
def build_factory(args):
    return lambda: Neo4jScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_service_args(parser)
    parser.add_argument("--user", default="neo4j")
    parser.add_argument("--password", default="testpassword")
    args = parser.parse_args()

    import neo4j
    print(
        f"case=neo4j mode={args.mode} neo4j={neo4j.__version__} "
        f"python={__import__('sys').version.split()[0]} host={args.host} port={args.port} "
        f"auth=basic user={args.user} round={args.round} delay={DELAY} bound={BOUND}",
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

    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    timeout = 12.0
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1("neo4j", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "neo4j", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "neo4j", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed(
                "neo4j", factory, reps=args.reps, timeout=timeout
            )
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
