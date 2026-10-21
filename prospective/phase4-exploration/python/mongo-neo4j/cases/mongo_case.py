"""pymongo 4.19.0 AsyncMongoClient pool cases (maxPoolSize=1).

Two reuse units on the same skeleton:

  txn    target = `start_session()` + (network) `start_transaction` first
         roundtrip + `replace_one(target)` inside the transaction + `commit`.
         The session and transaction state objects are kept on the scenario so
         the survivor can record what is still attached after a cancellation.
  plain  target = one ordinary `replace_one(target)` (implicit session, full
         command/reply roundtrip, no explicit transaction).

Survivor = same AsyncMongoClient (maxPoolSize=1), upserts the survivor doc in
the *same* collection and reads it back.  Observer = an independent
AsyncMongoClient, direct to the server (never through the proxy, never through
the tested client), reads the whole collection.

Pending mechanism: `DelayProxy` (retro `cases/common.py`, 50 ms per forwarded
server->client chunk) wrapping only the tested client.  Transactions require a
single-node replica set; the runner starts one.

Cancellation cleanup in the target is the documented explicit form:
`abort_transaction()` when still in a transaction, then `end_session()`.  The
state *before* that cleanup is recorded in `diag`; the judged observation only
contains the pool state at survivor acquire, the survivor's concrete write and
same-connection readback, and the independent observer's documents.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import inspect
import time

from pymongo import AsyncMongoClient

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

TARGET_DOC = "target"
SURVIVOR_DOC = "survivor"
B3_DOC = "b3"

TX_NAMES = {1: "NONE", 2: "STARTING", 3: "IN_PROGRESS", 4: "COMMITTED",
            5: "COMMITTED_EMPTY", 6: "ABORTED"}


def summarize_write(res):
    """A stable summary of a write result (no timestamps / election ids)."""
    if res[0] != "ok":
        return res
    r = res[1]
    return ("ok", {
        "matched": getattr(r, "matched_count", None),
        "modified": getattr(r, "modified_count", None),
        "upserted": None if getattr(r, "upserted_id", None) is None
                    else str(r.upserted_id),
        "ack": getattr(r, "acknowledged", None),
    })


class MongoScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.coll_name = unique_name("dw_r3_mongo")
        self.proxy: DelayProxy | None = None
        self.client: AsyncMongoClient | None = None
        self.observer: AsyncMongoClient | None = None
        self.session = None
        self.diag: dict = {}

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        via_proxy = f"mongodb://127.0.0.1:{client_port}/?directConnection=true"
        direct = f"mongodb://{a.host}:{a.port}/?directConnection=true"

        obs = AsyncMongoClient(
            direct, maxPoolSize=2, serverSelectionTimeoutMS=int(BOUND * 1000)
        )
        ping = await bound(obs.admin.command("ping"))
        if ping[0] != "ok":
            raise RuntimeError(f"observer connect failed: {ping}")
        self.observer = obs

        self.client = AsyncMongoClient(
            via_proxy,
            maxPoolSize=1,
            serverSelectionTimeoutMS=int(BOUND * 1000),
            connectTimeoutMS=int(BOUND * 1000),
        )
        warm = await bound(self.client.admin.command("ping"))
        if warm[0] != "ok":
            raise RuntimeError(f"tested client connect failed: {warm}")
        self.diag["setup_pool"] = self._pool()
        self.diag["setup_ids"] = self._pool_ids()

    @property
    def coll(self):
        return self.client[self.coll_name]["docs"]

    def _obs_coll(self):
        return self.observer[self.coll_name]["docs"]

    # ---- state probes -------------------------------------------------
    def _state(self):
        s = self.session
        if s is None:
            return None
        try:
            state = TX_NAMES.get(int(s._transaction.state), str(s._transaction.state))
        except BaseException:
            state = "?"
        try:
            ended = bool(s.has_ended)
        except BaseException:
            ended = None
        try:
            in_tx = bool(s.in_transaction)
        except BaseException:
            in_tx = None
        return {"tx": state, "in_tx": in_tx, "ended": ended}

    def _pool(self) -> dict:
        try:
            srv = next(iter(self.client._topology._servers.values()))
            pool = srv._pool
            return {
                "conns": len(pool.conns),
                "active": getattr(pool, "active_sockets", None),
                "max": pool.max_pool_size,
            }
        except BaseException as exc:
            return {"error": err_text(exc)}

    def _pool_ids(self) -> dict:
        try:
            srv = next(iter(self.client._topology._servers.values()))
            pool = srv._pool
            return {"ids": [id(c) for c in list(pool.conns)],
                    "active": getattr(pool, "active_sockets", None)}
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def _abort_if_open(self, s):
        if s is None:
            return "no_session"
        try:
            open_tx = s.in_transaction
        except BaseException as exc:
            return f"state_err {err_text(exc)}"
        if not open_tx:
            return "not_in_tx"
        return await bound(s.abort_transaction())

    async def _close_client(self, client) -> None:
        if client is None:
            return
        try:
            res = client.close()
            if inspect.isawaitable(res):
                await close_quiet(res)
        except asyncio.CancelledError:
            raise
        except BaseException:
            pass

    # ---- target ------------------------------------------------------
    async def _txn(self, doc_id: str, value: int) -> OpResult:
        """session -> start_transaction -> write -> commit, all explicit."""
        out: dict = {}
        s = self.client.start_session()
        self.session = s
        begin = ("err", "not started")
        write_raw = ("err", "not started")
        commit = ("err", "not started")
        try:
            out["session_id"] = s.session_id["id"].hex()
            begin = await bound(s.start_transaction())
            out["start_transaction"] = "ok" if begin[0] == "ok" else begin
            out["state_after_begin"] = self._state()
            if begin[0] == "ok":
                write_raw = await bound(
                    self.coll.replace_one(
                        {"_id": doc_id}, {"v": value}, upsert=True, session=s
                    )
                )
                out["write"] = summarize_write(write_raw)
                out["state_after_write"] = self._state()
                commit = await bound(s.commit_transaction())
                out["commit"] = "ok" if commit[0] == "ok" else commit
                out["state_after_commit"] = self._state()
        except BaseException as exc:
            out["escaped"] = err_text(exc)
            raise
        finally:
            self.diag["target_final"] = {
                "state": self._state(),
                "pool": self._pool(),
            }
            abort = await self._abort_if_open(s)
            end = await bound(s.end_session())
            self.diag["abort_if_open"] = abort
            self.diag["end_session"] = end
            self.diag["after_target"] = {
                "state": self._state(),
                "pool": self._pool(),
                "ids": self._pool_ids(),
            }
            # cleanup is best-effort and recorded, never a silent pass:
            out["cleanup"] = {"abort": abort, "end_session": end}
        return op_from(
            "txn",
            {"start_transaction": begin, "write": write_raw, "commit": commit},
            out,
        )

    async def _plain(self, doc_id: str, value: int) -> OpResult:
        write = await bound(
            self.coll.replace_one({"_id": doc_id}, {"v": value}, upsert=True)
        )
        out = {"write": summarize_write(write)}
        self.diag["after_target"] = {"pool": self._pool(), "ids": self._pool_ids()}
        return op_from("plain", {"write": write}, out)

    async def _update(self, doc_id: str, value: int) -> OpResult:
        update = await bound(
            self.coll.update_one({"_id": doc_id}, {"$set": {"v": value}}, upsert=True)
        )
        out = {"update": summarize_write(update)}
        self.diag["after_target"] = {"pool": self._pool(), "ids": self._pool_ids()}
        return op_from("plain", {"update": update}, out)

    async def target(self):
        op = (await self._txn(TARGET_DOC, 1) if self.mode == "txn"
              else await self._plain(TARGET_DOC, 1))
        if not op.ok:
            # An uncancelled target must never "complete" with a failed stage:
            # that would make post calibration look like pre (silent pass).
            raise RuntimeError(f"target failed at {op.stage}: {op.error}")
        return op

    async def same(self):
        return (await self._txn(TARGET_DOC, 1) if self.mode == "txn"
                else await self._plain(TARGET_DOC, 1))

    async def different(self):
        return (await self._plain(B3_DOC, 3) if self.mode == "txn"
                else await self._update(B3_DOC, 3))

    # ---- survivor / observer ----------------------------------------
    async def survivor(self):
        # Acquire-time state is diagnostic only.  A cancelled command may make
        # the driver discard the socket; that is a legal self-heal, not a data
        # divergence, and would otherwise turn a committed (Post) cancel into
        # Neither.  A *leaked* sole connection still shows up as a survivor
        # operation that blocks past its bound.
        out = {}
        # session state is diagnostic only: it cannot be part of the judged
        # observation because pre has no session object at all.
        self.diag["survivor_start"] = {
            "state": self._state() if self.mode == "txn" else "n/a",
            "pool": self._pool(),
            "ids": self._pool_ids(),
        }
        write = await bound(
            self.coll.replace_one({"_id": SURVIVOR_DOC}, {"v": 7}, upsert=True)
        )
        out["write"] = summarize_write(write)
        own = None
        if write[0] == "ok":
            own = await bound(
                self.coll.find_one({"_id": SURVIVOR_DOC}, {"v": 1})
            )
        out["own"] = own
        return op_from("survivor", {"write": write, "own": own}, out)

    async def observe(self):
        docs = await bound(
            self._obs_coll().find({}, {"v": 1}).sort("_id", 1).to_list(length=20)
        )
        if docs[0] != "ok":
            return OpResult(False, "observer", value={"docs": None},
                            error=f"{docs[0]}: {docs[1]}")
        rows = [[d["_id"], d.get("v")] for d in docs[1]]
        return OpResult(True, "observer", value={"docs": rows})

    # ---- cleanup -----------------------------------------------------
    async def close(self) -> None:
        await close_quiet(self._abort_if_open(self.session))
        if self.session is not None:
            await close_quiet(self.session.end_session())
        await self._close_client(self.client)
        await self._close_client(self.observer)
        if self.proxy is not None:
            await close_quiet(self.proxy.close())

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.coll_name)


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
            await bound(sc.client.admin.command("ping"))
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
    return lambda: MongoScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_service_args(parser)
    args = parser.parse_args()

    print(
        f"case=pymongo mode={args.mode} pymongo={getattr(__import__('pymongo'), 'version', '?')} "
        f"python={__import__('sys').version.split()[0]} host={args.host} port={args.port} "
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

    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    timeout = 12.0
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1("pymongo", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "pymongo", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "pymongo", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed(
                "pymongo", factory, reps=args.reps, timeout=timeout
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
