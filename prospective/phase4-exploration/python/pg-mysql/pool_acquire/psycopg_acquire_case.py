"""psycopg_pool 3.3.3: cancel a *waiting* ``AsyncConnectionPool.getconn()``.

Mechanism under test (installed source, not the docs):

``AsyncConnectionPool._getconn_unchecked()`` puts a ``WaitingClient`` into
``self._waiting`` and awaits ``pos.wait()``.  On ``CancelledError`` the
exception handler only returns a connection if ``pos.conn`` was already set; it
does **not** remove the waiter from ``_waiting``.  ``_add_to_pool()`` prunes
such waiters later (``WaitingClient.set()`` returns False when an error is
already set), so the connection should not be lost -- but the waiter state is
observable until the next return.

Two cancellation shapes:

A. queued: the only connection is held by setup, the acquire waits, cancel at
   its first suspension.  The holder is released by ``survivor()`` afterwards.
B. wake-up: setup holds the only connection; the target starts a releaser,
   waits, the releaser returns the connection, the pool pops it for the waiter
   and the waiter suspends next in the pool ``check`` callback; cancel there.
   The wake-up handoff is also raced directly (``--handoff``) by cancelling
   after ``putconn()`` returns but before the awakened waiter is scheduled.

Every acquire/release/observer/server operation is bounded at 2 s; a cut at an
unrealised suspension is recorded as such rather than counted as a finding.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import traceback

import psycopg_pool

from cases.common import BOUND, add_event, bound, close_quiet, err_text, must_ok
from dropwise_py.engine import _await_target

from .support import (
    PgObserver,
    clean_diag,
    pg_conninfo,
    pg_private,
    pg_stats,
    safe_pg_pid,
    safe_pg_status,
    target_record,
    unique_app,
    wait_until,
)


class PgAcquireScenario:
    def __init__(self, args, *, check_mode: str = "sleep", max_waiting: int = 0):
        assert check_mode in ("sleep", "none")
        self.args = args
        self.check_mode = check_mode
        self.max_waiting = max_waiting
        self.app = unique_app("pg-pool")
        self.obs_app = unique_app("pg-obs")
        self.pool = None
        self.holder = None
        self.holder_pid = None
        self.releaser = None
        self._completed_conn = None
        self.events: list[dict] = []
        self.diag: dict = {}
        self._closed = False

        self.observer = PgObserver(
            pg_conninfo(
                args.host, args.port, args.user, args.password, args.dbname,
                self.obs_app,
            )
        )
        self.conninfo = pg_conninfo(
            args.host, args.port, args.user, args.password, args.dbname, self.app
        )

    # ---- setup -------------------------------------------------------
    async def setup(self, pre_release: bool = False) -> None:
        await self.observer.connect()
        pre = await self.observer.sample(self.app)
        if pre.get("count") != 0:
            raise RuntimeError(f"observer pre-sample not empty: {pre!r}")

        pool = psycopg_pool.AsyncConnectionPool(
            self.conninfo,
            min_size=1,
            max_size=1,
            open=False,
            configure=self._configure,
            check=self._check if self.check_mode == "sleep" else None,
            timeout=BOUND,
            max_waiting=self.max_waiting,
            name=f"dw-pool-acquire-{self.app}",
        )
        self.pool = pool
        must_ok("pool open", await bound(pool.open(wait=True, timeout=BOUND)))

        warm = must_ok("warm-up acquire", await bound(pool.getconn(timeout=BOUND)))
        try:
            must_ok("warm-up select", await bound(self._select1(warm)))
        finally:
            must_ok("warm-up release", await bound(pool.putconn(warm)))

        holder = must_ok("holder acquire", await bound(pool.getconn(timeout=BOUND)))
        try:
            must_ok("holder select", await bound(self._select1(holder)))
        except BaseException:
            await close_quiet(pool.putconn(holder))
            raise
        self.holder = holder
        self.holder_pid = safe_pg_pid(holder)
        self.diag["holder_pid"] = self.holder_pid
        self.diag["app"] = self.app
        self.diag["obs_app"] = self.obs_app

        srv = await self.observe()
        stats = pg_stats(pool)
        self.diag["setup"] = {"stats": stats, "server": srv}
        if srv.get("count") != 1:
            raise RuntimeError(f"holder not visible server-side: {srv!r}")
        if stats.get("pool_size") != 1 or stats.get("pool_available") != 0:
            raise RuntimeError(f"unexpected setup stats: {stats!r}")
        if pre_release:
            self.diag["pre_release"] = await self.release_holder()

    def _configure(self, conn):
        return conn.set_autocommit(True)

    async def _check(self, conn) -> None:
        """Pool check hook: gives the wake-up cut a real suspension point."""
        self.diag["check_enters"] = self.diag.get("check_enters", 0) + 1
        add_event(
            self.events,
            "check_enter",
            pid=safe_pg_pid(conn),
            stats=pg_stats(self.pool),
            private=pg_private(self.pool),
        )
        try:
            await asyncio.sleep(0)
        except asyncio.CancelledError:
            self.diag["check_cancelled"] = True
            add_event(
                self.events,
                "check_cancelled",
                pid=safe_pg_pid(conn),
                stats=pg_stats(self.pool),
                private=pg_private(self.pool),
            )
            raise

    async def _select1(self, conn):
        cur = await conn.execute("SELECT 1")
        row = await cur.fetchone()
        return row[0] if row else None

    # ---- target ------------------------------------------------------
    async def target_queued(self):
        """Wait for the exhausted pool's only connection."""
        try:
            return await self.pool.getconn(timeout=BOUND)
        except asyncio.CancelledError:
            self._record_cancel("queued-or-check")
            raise

    async def target_wakeup(self, delay: float = 0.05):
        """Wait, then race the releaser; cancellation should land in check."""
        self.releaser = asyncio.create_task(self._release_later(delay))
        return await self.target_queued()

    def _record_cancel(self, where: str) -> None:
        self.diag["cancel_point"] = where
        self.diag["at_cancel"] = {
            "stats": pg_stats(self.pool),
            "private": pg_private(self.pool),
            "holder_held": self.holder is not None,
            "holder_pid": self.holder_pid,
            "check_enters": self.diag.get("check_enters", 0),
            "check_cancelled": self.diag.get("check_cancelled", False),
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
    async def release_holder(self, *, unwrapped: bool = False) -> dict:
        conn = self.holder
        if conn is None:
            return {"status": "already_released"}
        # Clear the slot first so a concurrent releaser/survivor cannot double
        # putconn the same handle.
        self.holder = None
        before = pg_stats(self.pool)
        try:
            if unwrapped:
                async with asyncio.timeout(BOUND):
                    await self.pool.putconn(conn)
                result = ("ok", None)
            else:
                result = await bound(self.pool.putconn(conn), BOUND)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            result = ("err", err_text(exc))
        out = {"status": result[0], "result": list(result), "stats_before": before,
               "stats_after": pg_stats(self.pool)}
        add_event(self.events, "holder_released", **out)
        return out

    async def release_conn(self, conn) -> dict:
        if conn is None:
            return {"status": "no_conn"}
        try:
            res = await bound(self.pool.putconn(conn), BOUND)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"status": "err", "error": err_text(exc)}
        return {"status": res[0], "result": list(res), "stats": pg_stats(self.pool)}

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
        return "ok" if task.exception() is None else f"err: {err_text(task.exception())}"

    async def survivor(self) -> dict:
        out: dict = {}
        out["completed_target_release"] = await self.release_conn(self._completed_conn)
        self._completed_conn = None
        out["releaser_join"] = await self._join_releaser()
        if self.holder is not None:
            out["holder_release"] = await self.release_holder()
        out["stats_before_acquire"] = pg_stats(self.pool)
        acq = await bound(self.pool.getconn(timeout=BOUND), BOUND)
        out["acquire"] = acq
        if acq[0] != "ok":
            out["stats_after_failed_acquire"] = pg_stats(self.pool)
            out["private_after_failed_acquire"] = pg_private(self.pool)
            return out
        conn = acq[1]
        out["pid"] = safe_pg_pid(conn)
        out["status"] = safe_pg_status(conn)
        out["stats_after_acquire"] = pg_stats(self.pool)
        out["query"] = await bound(self._select1(conn), BOUND)
        out["release"] = await bound(self.pool.putconn(conn), BOUND)
        out["stats_after_release"] = pg_stats(self.pool)
        out["server_after_survivor"] = await self.observe()
        return out

    async def ping_acquire(self) -> dict:
        acq = await bound(self.pool.getconn(timeout=BOUND), BOUND)
        if acq[0] != "ok":
            return {"acquire": acq, "stats": pg_stats(self.pool)}
        conn = acq[1]
        pid = safe_pg_pid(conn)
        query = await bound(self._select1(conn), BOUND)
        rel = await bound(self.pool.putconn(conn), BOUND)
        return {"pid": pid, "query": query, "release": list(rel),
                "stats": pg_stats(self.pool)}

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
            "stats": pg_stats(self.pool),
            "private": pg_private(self.pool),
            "holder_held": self.holder is not None,
            "server": await self.observe(),
        }

    # ---- cleanup -----------------------------------------------------
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
                diag["pool_close"] = await close_quiet(pool.close(timeout=1.0))
            await self.observer.close()
            self.diag["cleanup"] = diag


# ---------------------------------------------------------------------
# execution / evaluation
# ---------------------------------------------------------------------
def evaluate(rec: dict) -> dict:
    ev = {"issues": [], "checks": {}}
    target = rec.get("target") or {}
    shape = rec.get("shape", "")
    survivor = rec.get("survivor") or {}
    post = rec.get("post_cancel") or {}
    final = rec.get("final") or {}
    post_stats = post.get("stats") or {}
    post_private = post.get("private") or {}
    final_stats = final.get("stats") or {}
    final_private = final.get("private") or {}
    final_server = final.get("server") or {}
    surv_server = survivor.get("server_after_survivor") or {}
    surv_acq = survivor.get("acquire")
    surv_rel = survivor.get("release")

    no_cut_control = (
        (shape == "max_waiting" and rec.get("variant") == "no-cancel-control")
        or (shape == "free_conn" and rec.get("variant") == "no-check")
    )
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
        same_pid = survivor.get("pid") == rec.get("holder_pid")
        ev["checks"]["survivor_same_pid"] = same_pid
        if ok_acq and not same_pid:
            ev["issues"].append("survivor_pid_changed")

    ev["checks"]["post_waiting"] = post_stats.get("requests_waiting")
    ev["checks"]["post_private_waiting"] = post_private.get("waiting_len")
    ev["checks"]["post_available"] = post_stats.get("pool_available")
    ev["checks"]["post_server_count"] = (post.get("server") or {}).get("count")
    ev["checks"]["final_pool_size"] = final_stats.get("pool_size")
    ev["checks"]["final_available"] = final_stats.get("pool_available")
    ev["checks"]["final_waiting"] = final_stats.get("requests_waiting")
    ev["checks"]["final_private_waiting"] = final_private.get("waiting_len")
    ev["checks"]["final_server_count"] = final_server.get("count")
    ev["checks"]["final_server_pids"] = final_server.get("pids")

    if shape in ("A_queued", "B_check") and post_stats:
        # A queued cancellation is expected to leave a stale WaitingClient
        # until the holder is returned; that is recorded, then checked to clear.
        ev["checks"]["stale_waiter_at_cancel"] = bool(
            (post_private.get("waiting") or [{}])[0].get("error")
        ) if post_private.get("waiting") else False
        if shape == "B_check" and ev["checks"].get("stale_waiter_at_cancel"):
            ev["checks"]["handoff_waiter_was_stale"] = True

    if post_stats.get("connections_lost"):
        ev["issues"].append("connections_lost_nonzero")
    if final_stats.get("pool_size") != 1:
        ev["issues"].append("final_pool_size_not_1")
    if final_stats.get("pool_available") != 1:
        ev["issues"].append("final_available_not_1")
    if final_stats.get("requests_waiting") != 0:
        ev["issues"].append("final_waiting_not_0")
    if final_private.get("waiting_len") != 0:
        ev["issues"].append("final_private_waiting_nonzero")
    if final_server.get("count") != 1:
        ev["issues"].append(f"final_server_count_{final_server.get('count')}")
    if isinstance(final_server.get("pids"), list) and len(final_server["pids"]) > 1:
        ev["issues"].append("duplicate_server_backends")
    if surv_server and surv_server.get("count") not in (1, None):
        ev["issues"].append(f"survivor_server_count_{surv_server.get('count')}")
    ev["clean"] = not ev["issues"]
    return ev


async def _run(args, rec: dict, sc: PgAcquireScenario, coro_factory) -> None:
    try:
        await asyncio.wait_for(sc.setup(pre_release=rec.get("pre_release", False)), 12)
        rec["holder_pid"] = sc.holder_pid
        rec["pre"] = await sc.snapshot("pre")
        await coro_factory(sc, rec)
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


async def _cut_body(sc: PgAcquireScenario, rec: dict) -> None:
    target = sc.target_wakeup() if rec["shape"] == "B_check" else sc.target_queued()
    tr = await _await_target(target, rec["k"], 6)
    rec["target"] = target_record(tr)
    if tr.outcome == "completed" and tr.result is not None:
        # Unrealised cut or control: the acquire succeeded; hand the connection
        # back before any further diagnostic (never inspect it after release).
        rec["completed_target_pid"] = safe_pg_pid(tr.result)
        rec["completed_target_release"] = await sc.release_conn(tr.result)
    elif tr.result is not None:
        # Cancelled acquire should not have produced a connection; never keep it.
        rec["unexpected_target_result"] = str(tr.result)
    rec["post_cancel"] = await sc.snapshot("post_cancel")
    await asyncio.sleep(0.1)
    rec["post_cancel_settled"] = await sc.snapshot("post_cancel_settled")
    rec["survivor"] = await asyncio.wait_for(sc.survivor(), 12)


async def _normal_body(sc: PgAcquireScenario, rec: dict) -> None:
    rec["shape"] = "normal"
    rec["holder_release"] = await sc.release_holder()
    pings = []
    for _ in range(3):
        pings.append(await sc.ping_acquire())
    rec["pings"] = pings
    rec["survivor"] = await sc.survivor()


async def _uncancelled_wait_body(sc: PgAcquireScenario, rec: dict) -> None:
    tr = await _await_target(sc.target_wakeup(), None, 6)
    rec["target"] = target_record(tr)
    if tr.outcome == "completed" and tr.result is not None:
        rec["completed_target_pid"] = safe_pg_pid(tr.result)
        rec["completed_target_release"] = await sc.release_conn(tr.result)
    rec["post_cancel"] = await sc.snapshot("post_target")
    rec["survivor"] = await asyncio.wait_for(sc.survivor(), 12)


async def _handoff_body(sc: PgAcquireScenario, rec: dict) -> None:
    """Cancel after putconn() but before the awakened waiter is scheduled."""
    task = asyncio.create_task(sc.pool.getconn(timeout=BOUND))
    ok, polls, elapsed = await wait_until(lambda: len(sc.pool._waiting) == 1, 1.0)
    rec["queued"] = {"ok": ok, "polls": polls, "seconds": round(elapsed, 6)}
    if not ok:
        task.cancel()
        await close_quiet(task)
        rec["exit_early"] = True
        rec["error"] = "acquire never became queued"
        return
    pos = sc.pool._waiting[0]
    rec["before_release"] = {
        "waiting_len": len(sc.pool._waiting),
        "pos_has_conn": pos.conn is not None,
        "pos_error": type(pos.error).__name__ if pos.error else None,
        "target_done": task.done(),
    }
    rec["holder_release"] = await sc.release_holder(unwrapped=True)
    pos_conn = pos.conn is not None
    rec["wakeup_window"] = {
        "target_done_after_release": task.done(),
        "pos_has_conn": pos_conn,
        "waiting_len": len(sc.pool._waiting),
        "stats": pg_stats(sc.pool),
        "private": pg_private(sc.pool),
    }
    if task.done():
        # putconn yielded: the waiter already resumed.  Not a handoff-window cut.
        rec["wakeup_window"]["missed"] = True
        if not task.cancelled():
            try:
                conn = task.result()
            except BaseException as exc:
                conn = None
                rec["wakeup_window"]["target_error"] = err_text(exc)
            if conn is not None:
                rec["completed_target_pid"] = safe_pg_pid(conn)
                rec["completed_target_release"] = await sc.release_conn(conn)
    else:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            rec["wakeup_window"]["cancelled"] = True
        except BaseException as exc:
            rec["wakeup_window"]["cancel_error"] = err_text(exc)
        rec["wakeup_window"]["pos_error_after_cancel"] = (
            type(pos.error).__name__ if pos.error else None
        )
        rec["wakeup_window"]["path"] = (
            "pos_conn_return_task" if pos_conn else "free_list_direct"
        )
    # The ReturnConnection maintenance task is asynchronous.  Observe, bounded.
    if pos_conn:
        ok2, polls2, elapsed2 = await wait_until(
            lambda: pg_stats(sc.pool).get("pool_available") == 1, 1.0
        )
        rec["wakeup_window"]["return_observed"] = {
            "ok": ok2, "polls": polls2, "seconds": round(elapsed2, 6),
            "stats": pg_stats(sc.pool),
        }
    cancelled_here = bool(rec["wakeup_window"].get("cancelled"))
    rec["target"] = {
        "outcome": "cancelled" if cancelled_here else "completed",
        "realized": cancelled_here,
        "manual_handoff": True,
    }
    rec["post_cancel"] = await sc.snapshot("post_cancel")
    rec["survivor"] = await asyncio.wait_for(sc.survivor(), 12)


async def _maxwait_body(sc: PgAcquireScenario, rec: dict) -> None:
    """max_waiting=1: a cancelled waiter may consume the waiting allowance."""
    tr = await _await_target(sc.target_queued(), 1, 6)
    rec["target"] = target_record(tr)
    rec["post_cancel"] = await sc.snapshot("post_cancel")
    rec["private_at_probe"] = pg_private(sc.pool)
    try:
        conn = await asyncio.wait_for(sc.pool.getconn(timeout=0.5), 1.0)
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        rec["second_acquire"] = {"error": type(exc).__name__, "text": str(exc)}
    else:
        rec["second_acquire"] = {"ok": True, "pid": safe_pg_pid(conn)}
        await sc.release_conn(conn)
    rec["survivor"] = await asyncio.wait_for(sc.survivor(), 12)


async def _maxwait_control_body(sc: PgAcquireScenario, rec: dict) -> None:
    """max_waiting=1 without cancellation: one real waiter queues and is served."""
    task = asyncio.create_task(sc.pool.getconn(timeout=BOUND))
    ok, polls, elapsed = await wait_until(lambda: len(sc.pool._waiting) == 1, 1.0)
    rec["waiter"] = {"ok": ok, "polls": polls, "seconds": round(elapsed, 6),
                     "stats": pg_stats(sc.pool), "private": pg_private(sc.pool)}
    rec["holder_release"] = await sc.release_holder()
    try:
        conn = await asyncio.wait_for(task, BOUND)
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        rec["waiter"]["error"] = err_text(exc)
    else:
        rec["waiter"]["pid"] = safe_pg_pid(conn)
        rec["waiter"]["release"] = await sc.release_conn(conn)
    rec["post_cancel"] = await sc.snapshot("post_target")
    rec["survivor"] = None


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
        f"case=psycopg-pool-acquire psycopg={__import__('psycopg').__version__} "
        f"psycopg_pool={psycopg_pool.__version__} "
        f"python={sys.version.split()[0]} host={args.host} port={args.port} "
        f"bound={BOUND}",
        flush=True,
    )
    records: list[dict] = []
    reps = args.reps

    def add(rec, sc, body, **kw):
        rec.update(kw)
        records.append(rec)
        return _run(args, rec, sc, body)

    for rep in range(1, reps + 1):
        await add(
            {"unit": "psycopg_pool", "shape": "A_queued", "k": 1, "rep": rep},
            PgAcquireScenario(args, check_mode="sleep"), _cut_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "psycopg_pool", "shape": "A_queued", "k": 2, "rep": rep},
            PgAcquireScenario(args, check_mode="sleep"), _cut_body,
        )
    for rep in range(1, reps + 1):
        await add(
            {"unit": "psycopg_pool", "shape": "B_check", "k": 2, "rep": rep},
            PgAcquireScenario(args, check_mode="sleep"), _cut_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "psycopg_pool", "shape": "B_check", "k": 1, "rep": rep},
            PgAcquireScenario(args, check_mode="sleep"), _cut_body,
        )
    for rep in range(1, reps + 1):
        await add(
            {"unit": "psycopg_pool", "shape": "B_handoff", "k": None, "rep": rep,
             "variant": "manual"},
            PgAcquireScenario(args, check_mode="none"), _handoff_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "psycopg_pool", "shape": "free_conn", "k": 1, "rep": rep,
             "variant": "check", "pre_release": True},
            PgAcquireScenario(args, check_mode="sleep"), _cut_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "psycopg_pool", "shape": "free_conn", "k": 1, "rep": rep,
             "variant": "no-check", "pre_release": True},
            PgAcquireScenario(args, check_mode="none"), _cut_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "psycopg_pool", "shape": "normal", "k": None, "rep": rep,
             "variant": "check-none"},
            PgAcquireScenario(args, check_mode="none"), _normal_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "psycopg_pool", "shape": "uncancelled_wait", "k": None,
             "rep": rep, "variant": "check"},
            PgAcquireScenario(args, check_mode="sleep"), _uncancelled_wait_body,
        )
    for rep in range(1, 4):
        await add(
            {"unit": "psycopg_pool", "shape": "max_waiting", "k": 1, "rep": rep,
             "variant": "cancel-then-new-acquire"},
            PgAcquireScenario(args, check_mode="none", max_waiting=1),
            _maxwait_body,
        )
    for rep in range(1, 3):
        await add(
            {"unit": "psycopg_pool", "shape": "max_waiting", "k": None, "rep": rep,
             "variant": "no-cancel-control"},
            PgAcquireScenario(args, check_mode="none", max_waiting=1),
            _maxwait_control_body,
        )

    summarize(records)


def main() -> None:
    args = build_parser(__doc__).parse_args()
    asyncio.run(main_async(args))


if __name__ == "__main__":
    main()
