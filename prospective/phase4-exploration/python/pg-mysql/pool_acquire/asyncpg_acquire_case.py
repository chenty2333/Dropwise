"""asyncpg 0.32.0: cancel a *waiting* ``Pool.acquire()``.

Mechanism under test (installed source, not the docs):

``Pool._acquire()`` awaits ``self._queue.get()``, then ``ch.acquire()``.
Cancellation while the queue get is pending leaves the holder in the queue
(asyncio.Queue's get cleans up without removing an item).  Cancellation after
the holder was popped (the wake-up handoff) is caught by
``PoolConnectionHolder.acquire()``'s ``(Exception, CancelledError)`` handlers:
a failing/cancelled ``setup`` closes the raw connection, and ``_acquire_impl``
puts the holder back into the queue.  A cancelled holder is therefore expected
to be replaced on the next acquire, not duplicated.

Two cancellation shapes:

A. queued: setup holds the only connection; the acquire waits on the queue;
   cancel at its first suspension; ``survivor()`` releases the holder and
   acquires again.
B. wake-up: setup holds the only connection; the target starts a releaser;
   the releaser returns it, the waiter pops the holder and suspends in the
   ``setup`` callback; cancel there.  The holder is expected to be returned to
   the queue (disconnected); the next acquire reconnects a new backend.

Cancelling after the pop with *no* setup callback does not suspend before
``acquire()`` returns, so it is not a reachable cut through the public Pool
API; that limit is reported rather than simulated with internals.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import traceback

import asyncpg

from cases.common import BOUND, add_event, bound, close_quiet, err_text, must_ok
from dropwise_py.engine import _await_target

from .support import (
    ApgObserver,
    apg_dsn,
    apg_stats,
    clean_diag,
    safe_apg_pid,
    target_record,
    unique_app,
    wait_until,
)


class ApgAcquireScenario:
    def __init__(self, args, *, setup_mode: str = "sleep"):
        assert setup_mode in ("sleep", "none")
        self.args = args
        self.setup_mode = setup_mode
        self.app = unique_app("apg-pool")
        self.obs_app = unique_app("apg-obs")
        self.pool = None
        self.holder = None
        self.holder_pid = None
        self.releaser = None
        self._completed_conn = None
        self.events: list[dict] = []
        self.diag: dict = {}
        self._closed = False

        self.observer = ApgObserver(
            apg_dsn(args.host, args.port, args.user, args.password, args.dbname,
                    self.obs_app)
        )
        self.dsn = apg_dsn(
            args.host, args.port, args.user, args.password, args.dbname, self.app
        )

    # ---- setup -------------------------------------------------------
    async def setup(self, pre_release: bool = False) -> None:
        await self.observer.connect()
        pre = await self.observer.sample(self.app)
        if pre.get("count") != 0:
            raise RuntimeError(f"observer pre-sample not empty: {pre!r}")
        self.pool = must_ok(
            "pool create",
            await bound(
                asyncpg.create_pool(
                    self.dsn,
                    min_size=1,
                    max_size=1,
                    timeout=BOUND,
                    setup=self._setup_cb if self.setup_mode == "sleep" else None,
                ),
                BOUND,
            ),
        )
        warm = must_ok("warm-up acquire", await bound(self.pool.acquire(timeout=BOUND)))
        try:
            must_ok("warm-up select", await bound(warm.fetchval("SELECT 1")))
        finally:
            must_ok("warm-up release", await bound(self.pool.release(warm)))

        holder = must_ok("holder acquire", await bound(self.pool.acquire(timeout=BOUND)))
        try:
            must_ok("holder select", await bound(holder.fetchval("SELECT 1")))
        except BaseException:
            await close_quiet(self.pool.release(holder))
            raise
        self.holder = holder
        self.holder_pid = safe_apg_pid(holder)
        self.diag["holder_pid"] = self.holder_pid
        self.diag["app"] = self.app
        self.diag["obs_app"] = self.obs_app
        srv = await self.observe()
        stats = apg_stats(self.pool)
        self.diag["setup"] = {"stats": stats, "server": srv}
        if srv.get("count") != 1:
            raise RuntimeError(f"holder not visible server-side: {srv!r}")
        if stats.get("size") != 1 or stats.get("qsize") != 0:
            raise RuntimeError(f"unexpected setup stats: {stats!r}")
        if pre_release:
            self.diag["pre_release"] = await self.release_holder()

    async def _setup_cb(self, proxy) -> None:
        """Setup hook: gives the wake-up cut a real suspension point."""
        self.diag["setup_enters"] = self.diag.get("setup_enters", 0) + 1
        add_event(
            self.events,
            "setup_enter",
            pid=safe_apg_pid(proxy),
            stats=apg_stats(self.pool),
        )
        try:
            await asyncio.sleep(0)
        except asyncio.CancelledError:
            self.diag["setup_cancelled"] = True
            add_event(
                self.events,
                "setup_cancelled",
                pid=safe_apg_pid(proxy),
                stats=apg_stats(self.pool),
            )
            raise

    # ---- target ------------------------------------------------------
    async def target_queued(self):
        try:
            return await self.pool.acquire(timeout=BOUND)
        except asyncio.CancelledError:
            self._record_cancel("queued-or-setup")
            raise

    async def target_wakeup(self, delay: float = 0.05):
        self.releaser = asyncio.create_task(self._release_later(delay))
        return await self.target_queued()

    def _record_cancel(self, where: str) -> None:
        self.diag["cancel_point"] = where
        self.diag["at_cancel"] = {
            "stats": apg_stats(self.pool),
            "holder_held": self.holder is not None,
            "holder_pid": self.holder_pid,
            "setup_enters": self.diag.get("setup_enters", 0),
            "setup_cancelled": self.diag.get("setup_cancelled", False),
        }
        add_event(self.events, "target_cancelled", **self.diag["at_cancel"])

    async def _release_later(self, delay: float) -> None:
        try:
            await asyncio.sleep(delay)
            self.diag["releaser"] = await self.release_holder()
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            self.diag["releaser_error"] = err_text(exc)
            add_event(self.events, "releaser_error", error=err_text(exc))

    # ---- release / survivor -----------------------------------------
    async def release_holder(self) -> dict:
        conn = self.holder
        if conn is None:
            return {"status": "already_released"}
        self.holder = None
        before = apg_stats(self.pool)
        try:
            async with asyncio.timeout(BOUND):
                await self.pool.release(conn)
            result = ("ok", None)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            result = ("err", err_text(exc))
        out = {"status": result[0], "result": list(result), "stats_before": before,
               "stats_after": apg_stats(self.pool)}
        add_event(self.events, "holder_released", **out)
        return out

    async def release_conn(self, conn) -> dict:
        if conn is None:
            return {"status": "no_conn"}
        try:
            res = await bound(self.pool.release(conn), BOUND)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"status": "err", "error": err_text(exc)}
        return {"status": res[0], "result": list(res), "stats": apg_stats(self.pool)}

    async def _join_releaser(self) -> str:
        task, self.releaser = self.releaser, None
        if task is None:
            return "none"
        if not task.done():
            try:
                await asyncio.wait_for(task, BOUND)
            except asyncio.CancelledError:
                raise
            except BaseException as exc:
                return f"err: {err_text(exc)}"
        try:
            exc = task.exception()
        except asyncio.CancelledError:
            return "cancelled"
        return "ok" if exc is None else f"err: {err_text(exc)}"

    async def survivor(self) -> dict:
        out: dict = {}
        out["completed_target_release"] = await self.release_conn(self._completed_conn)
        self._completed_conn = None
        out["releaser_join"] = await self._join_releaser()
        if self.holder is not None:
            out["holder_release"] = await self.release_holder()
        out["stats_before_acquire"] = apg_stats(self.pool)
        acq = await bound(self.pool.acquire(timeout=BOUND), BOUND)
        out["acquire"] = acq
        if acq[0] != "ok":
            out["stats_after_failed_acquire"] = apg_stats(self.pool)
            return out
        conn = acq[1]
        out["pid"] = safe_apg_pid(conn)
        out["stats_after_acquire"] = apg_stats(self.pool)
        out["query"] = await bound(conn.fetchval("SELECT 1"), BOUND)
        out["release"] = await bound(self.pool.release(conn), BOUND)
        out["stats_after_release"] = apg_stats(self.pool)
        out["server_after_survivor"] = await self.observe()
        return out

    async def ping_acquire(self) -> dict:
        acq = await bound(self.pool.acquire(timeout=BOUND), BOUND)
        if acq[0] != "ok":
            return {"acquire": acq, "stats": apg_stats(self.pool)}
        conn = acq[1]
        pid = safe_apg_pid(conn)
        query = await bound(conn.fetchval("SELECT 1"), BOUND)
        rel = await bound(self.pool.release(conn), BOUND)
        return {"pid": pid, "query": query, "release": list(rel),
                "stats": apg_stats(self.pool)}

    async def observe(self) -> dict:
        try:
            sample = await self.observer.sample(self.app)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            sample = {"error": err_text(exc)}
        add_event(self.events, "server_sample", sample=sample)
        return sample

    async def snapshot(self, tag: str) -> dict:
        return {
            "tag": tag,
            "stats": apg_stats(self.pool),
            "holder_held": self.holder is not None,
            "server": await self.observe(),
        }

    async def cleanup(self) -> None:
        if self._closed:
            return
        self._closed = True
        diag: dict = {}
        try:
            diag["releaser_join"] = await self._join_releaser()
            if self.holder is not None:
                diag["holder_release"] = await self.release_holder()
            if self._completed_conn is not None:
                diag["completed_target_release"] = await self.release_conn(
                    self._completed_conn
                )
                self._completed_conn = None
        finally:
            if self.pool is not None:
                pool, self.pool = self.pool, None
                diag["pool_close"] = await close_quiet(pool.close())
            await self.observer.close()
            self.diag["cleanup"] = diag


# ---------------------------------------------------------------------
def evaluate(rec: dict) -> dict:
    ev = {"issues": [], "checks": {}}
    target = rec.get("target") or {}
    shape = rec.get("shape", "")
    survivor = rec.get("survivor") or {}
    final = rec.get("final") or {}
    post = rec.get("post_cancel") or {}
    post_stats = post.get("stats") or {}
    final_stats = final.get("stats") or {}
    final_server = final.get("server") or {}
    surv_server = survivor.get("server_after_survivor") or {}
    surv_acq = survivor.get("acquire")
    surv_rel = survivor.get("release")

    no_cut_control = shape == "free_conn" and rec.get("variant") == "no-setup"
    is_cut = shape not in ("normal", "uncancelled_wait") and not no_cut_control
    if is_cut:
        ev["checks"]["cut_realized"] = bool(target.get("realized"))
        if not target.get("realized"):
            ev["issues"].append("cut_unrealized")

    if survivor:
        ok_acq = isinstance(surv_acq, (tuple, list)) and len(surv_acq) > 0 and surv_acq[0] == "ok"
        ok_rel = isinstance(surv_rel, (tuple, list)) and len(surv_rel) > 0 and surv_rel[0] == "ok"
        ev["checks"]["survivor_acquire_ok"] = ok_acq
        ev["checks"]["survivor_release_ok"] = ok_rel
        ev["checks"]["survivor_query"] = survivor.get("query")
        if not ok_acq:
            ev["issues"].append("survivor_acquire_failed")
        if not ok_rel:
            ev["issues"].append("survivor_release_failed")
        if survivor.get("query") not in (("ok", 1), ["ok", 1]):
            ev["issues"].append("survivor_query_failed")

    ev["checks"]["post_size"] = post_stats.get("size")
    ev["checks"]["post_idle"] = post_stats.get("idle")
    ev["checks"]["post_qsize"] = post_stats.get("qsize")
    ev["checks"]["post_server_count"] = (post.get("server") or {}).get("count")
    ev["checks"]["final_size"] = final_stats.get("size")
    ev["checks"]["final_idle"] = final_stats.get("idle")
    ev["checks"]["final_qsize"] = final_stats.get("qsize")
    ev["checks"]["final_server_count"] = final_server.get("count")
    ev["checks"]["final_server_pids"] = final_server.get("pids")

    if final_stats.get("size") != 1:
        ev["issues"].append("final_size_not_1")
    if final_stats.get("qsize") != 1:
        ev["issues"].append("final_qsize_not_1")
    if final_server.get("count") != 1:
        ev["issues"].append(f"final_server_count_{final_server.get('count')}")
    if isinstance(final_server.get("pids"), list) and len(final_server["pids"]) > 1:
        ev["issues"].append("duplicate_server_backends")
    if surv_server and surv_server.get("count") not in (1, None):
        ev["issues"].append(f"survivor_server_count_{surv_server.get('count')}")
    ev["clean"] = not ev["issues"]
    return ev


async def _run(args, rec: dict, sc: ApgAcquireScenario, body) -> None:
    try:
        await asyncio.wait_for(sc.setup(pre_release=rec.get("pre_release", False)), 12)
        rec["holder_pid"] = sc.holder_pid
        rec["pre"] = await sc.snapshot("pre")
        await body(sc, rec)
        if rec.get("exit_early"):
            return
        rec["final"] = await sc.snapshot("final")
        rec["evaluation"] = evaluate(rec)
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        rec["error"] = err_text(exc)
        rec["traceback"] = traceback.format_exc()[-1600:]
    finally:
        try:
            await asyncio.wait_for(sc.cleanup(), 12)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            rec["cleanup_error"] = err_text(exc)
        rec["diag"] = sc.diag
        rec["events"] = sc.events[-40:]
        print("EXEC " + json.dumps(clean_diag(rec), default=str, sort_keys=True),
              flush=True)


async def _cut_body(sc: ApgAcquireScenario, rec: dict) -> None:
    target = sc.target_wakeup() if rec["shape"] == "B_setup" else sc.target_queued()
    tr = await _await_target(target, rec["k"], 6)
    rec["target"] = target_record(tr)
    if tr.outcome == "completed" and tr.result is not None:
        rec["completed_target_pid"] = safe_apg_pid(tr.result)
        rec["completed_target_release"] = await sc.release_conn(tr.result)
    elif tr.result is not None:
        rec["unexpected_target_result"] = str(tr.result)
    rec["post_cancel"] = await sc.snapshot("post_cancel")
    await asyncio.sleep(0.1)
    rec["post_cancel_settled"] = await sc.snapshot("post_cancel_settled")
    rec["survivor"] = await asyncio.wait_for(sc.survivor(), 12)


async def _normal_body(sc: ApgAcquireScenario, rec: dict) -> None:
    rec["shape"] = "normal"
    rec["holder_release"] = await sc.release_holder()
    pings = []
    for _ in range(3):
        pings.append(await sc.ping_acquire())
    rec["pings"] = pings
    rec["survivor"] = await sc.survivor()


async def _uncancelled_wait_body(sc: ApgAcquireScenario, rec: dict) -> None:
    tr = await _await_target(sc.target_wakeup(), None, 6)
    rec["target"] = target_record(tr)
    if tr.outcome == "completed" and tr.result is not None:
        rec["completed_target_pid"] = safe_apg_pid(tr.result)
        rec["completed_target_release"] = await sc.release_conn(tr.result)
    rec["post_cancel"] = await sc.snapshot("post_target")
    rec["survivor"] = await asyncio.wait_for(sc.survivor(), 12)


# ---------------------------------------------------------------------
def build_parser(description: str) -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=description)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--reps", type=int, default=5)
    return p


def summarize(records: list[dict]) -> None:
    groups: dict[tuple, list[dict]] = {}
    for rec in records:
        key = (rec.get("shape"), rec.get("k"), rec.get("variant"))
        groups.setdefault(key, []).append(rec)
    print("=== SUMMARY ===", flush=True)
    for key in sorted(groups, key=lambda k: (str(k[0]), str(k[1]), str(k[2]))):
        rows = groups[key]
        realized = sum(1 for r in rows if (r.get("target") or {}).get("realized"))
        clean = sum(1 for r in rows if (r.get("evaluation") or {}).get("clean"))
        errors = sum(1 for r in rows if r.get("error") or r.get("cleanup_error"))
        issues: dict[str, int] = {}
        for r in rows:
            for issue in ((r.get("evaluation") or {}).get("issues") or []):
                issues[issue] = issues.get(issue, 0) + 1
        print(
            f"SHAPE shape={key[0]} k={key[1]} variant={key[2]} "
            f"executions={len(rows)} realized={realized} clean={clean} "
            f"errors={errors} issues={json.dumps(issues, sort_keys=True)}",
            flush=True,
        )
    print(f"TOTAL executions={len(records)}", flush=True)


async def main_async(args) -> None:
    print(
        f"case=asyncpg-pool-acquire asyncpg={asyncpg.__version__} "
        f"python={sys.version.split()[0]} host={args.host} port={args.port} "
        f"bound={BOUND}",
        flush=True,
    )
    records: list[dict] = []
    reps = args.reps

    async def add(rec, sc, body, **kw):
        rec.update(kw)
        records.append(rec)
        await _run(args, rec, sc, body)

    for rep in range(1, reps + 1):
        await add(
            {"unit": "asyncpg_pool", "shape": "A_queued", "k": 1, "rep": rep},
            ApgAcquireScenario(args, setup_mode="sleep"), _cut_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "asyncpg_pool", "shape": "A_queued", "k": 2, "rep": rep},
            ApgAcquireScenario(args, setup_mode="sleep"), _cut_body,
        )
    for rep in range(1, reps + 1):
        await add(
            {"unit": "asyncpg_pool", "shape": "B_setup", "k": 2, "rep": rep},
            ApgAcquireScenario(args, setup_mode="sleep"), _cut_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "asyncpg_pool", "shape": "B_setup", "k": 1, "rep": rep},
            ApgAcquireScenario(args, setup_mode="sleep"), _cut_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "asyncpg_pool", "shape": "free_conn", "k": 1, "rep": rep,
             "variant": "setup", "pre_release": True},
            ApgAcquireScenario(args, setup_mode="sleep"), _cut_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "asyncpg_pool", "shape": "free_conn", "k": 1, "rep": rep,
             "variant": "no-setup", "pre_release": True},
            ApgAcquireScenario(args, setup_mode="none"), _cut_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "asyncpg_pool", "shape": "normal", "k": None, "rep": rep,
             "variant": "no-setup"},
            ApgAcquireScenario(args, setup_mode="none"), _normal_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "asyncpg_pool", "shape": "uncancelled_wait", "k": None,
             "rep": rep, "variant": "setup"},
            ApgAcquireScenario(args, setup_mode="sleep"), _uncancelled_wait_body,
        )

    summarize(records)


def main() -> None:
    args = build_parser(__doc__).parse_args()
    asyncio.run(main_async(args))


if __name__ == "__main__":
    main()
