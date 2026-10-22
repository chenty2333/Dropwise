"""Cancel a mature pool's acquire *delivery window*, with a live user present.

The window under test is narrower than "waiting for a connection":

    the pool has already assigned a connection/holder to this caller,
    but the caller has not yet received the lease/guard.

psycopg_pool: the connection is popped from the free list or assigned to
``WaitingClient.conn``; ``_getconn_with_check_loop`` then awaits the configured
``check`` callback.  Cancellation there is caught by ``except BaseException``
and the connection is returned with ``_putconn(..., from_getconn=True)``.

asyncpg: ``PoolConnectionHolder.acquire`` has already popped the holder and is
awaiting the user ``setup`` callback.  Cancellation closes the raw connection
and the holder is put back by ``_acquire_impl``'s handler.

A second, independent user holds one connection throughout and keeps session
state (a GUC and a temp table).  The test asks, after the cancelled delivery:

* can a subsequent caller acquire and query?
* is capacity restored (pool size/available, server sessions)?
* is the delivered connection identity returned/renumbered?
* is the live user's session and query still intact?

Controls on the same pool: normal delivery, cancellation before the connection
is assigned (early), and cancellation after successful delivery while a query
is running (late, through the context manager so the lease is returned).

This is a mature-implementation positive-control experiment.  It does not
assume a defect; a clean result is the expected protection and is reported as
such.  No pool source is modified.  The only hooks are the public ``check`` /
``setup`` callbacks and read-only stats/private snapshots used as diagnostics.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import sys
import time

import asyncpg
import psycopg_pool

from cases.common import BOUND, bound, close_quiet, err_text
from dropwise_py.engine import _await_target

from pool_acquire.support import (
    ApgObserver,
    PgObserver,
    apg_dsn,
    apg_stats,
    pg_conninfo,
    pg_private,
    pg_stats,
    safe_apg_pid,
    safe_pg_pid,
    unique_app,
    wait_until,
)


class DeliveryScenario:
    def __init__(self, args, shape: str, rep: int) -> None:
        self.args = args
        self.driver = args.driver
        self.shape = shape
        self.rep = rep
        self.app = unique_app("dw-delivery")
        self.obs_app = unique_app("dw-delivery-obs")
        self.pool = None
        self.legit = None
        self.legit_pid = None
        self.delivered_pid = None
        self.check_entered = asyncio.Event()
        self.check_release = asyncio.Event()
        self.block_check = False
        self.events: list[dict] = []
        self.rec: dict = {"driver": self.driver, "shape": shape, "rep": rep,
                          "app": self.app}
        if self.driver == "psycopg":
            self.observer = PgObserver(
                pg_conninfo(args.host, args.port, args.user, args.password,
                            args.dbname, self.obs_app)
            )
            self.conninfo = pg_conninfo(
                args.host, args.port, args.user, args.password, args.dbname,
                self.app,
            )
        else:
            self.observer = ApgObserver(
                apg_dsn(args.host, args.port, args.user, args.password,
                        args.dbname, self.obs_app)
            )
            self.dsn = apg_dsn(
                args.host, args.port, args.user, args.password, args.dbname,
                self.app,
            )

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        await self.observer.connect()
        pre = await self.observer.sample(self.app)
        if pre.get("count") != 0:
            raise RuntimeError(f"observer pre-sample not empty: {pre!r}")
        if self.driver == "psycopg":
            self.pool = psycopg_pool.AsyncConnectionPool(
                self.conninfo,
                min_size=1,
                max_size=2,
                open=False,
                configure=self._configure,
                check=self._check,
                timeout=BOUND,
                name=f"dw-delivery-{self.app}",
            )
            await self.pool.open(wait=True, timeout=BOUND)
        else:
            self.pool = await asyncpg.create_pool(
                self.dsn, min_size=1, max_size=2, timeout=BOUND,
                setup=self._setup_cb,
            )
        # One legitimate user holds one of the two allowed connections.
        self.legit = await self._acquire()
        self.legit_pid = await self._pid(self.legit)
        await self._exec(self.legit, "SET dw.legit = 'keeper'")
        await self._exec(self.legit, "CREATE TEMP TABLE dw_legit_tmp (x int)")
        await self._exec(self.legit, "INSERT INTO dw_legit_tmp VALUES (7)")
        self.rec["setup"] = {
            "legit_pid": self.legit_pid,
            "stats": self._stats(),
            "server": await self.observer.sample(self.app),
        }
        if self.rec["setup"]["server"].get("count") != 1:
            raise RuntimeError(f"legit user not visible: {self.rec['setup']!r}")

    async def _configure(self, conn):
        await conn.set_autocommit(True)

    async def _check(self, conn) -> None:
        """psycopg_pool check callback: a real suspension at delivery."""
        if not self.block_check:
            return
        self.delivered_pid = safe_pg_pid(conn)
        self.rec.setdefault("check_enters", 0)
        self.rec["check_enters"] += 1
        self.rec["check_pid"] = self.delivered_pid
        self.check_entered.set()
        try:
            await self.check_release.wait()
        except asyncio.CancelledError:
            self.rec["check_cancelled"] = True
            raise

    async def _setup_cb(self, proxy) -> None:
        """asyncpg pool setup callback: a real suspension at delivery."""
        if not self.block_check:
            return
        self.delivered_pid = safe_apg_pid(proxy)
        self.rec.setdefault("setup_enters", 0)
        self.rec["setup_enters"] += 1
        self.rec["setup_pid"] = self.delivered_pid
        self.check_entered.set()
        try:
            await self.check_release.wait()
        except asyncio.CancelledError:
            self.rec["setup_cancelled"] = True
            raise

    # ---- small helpers ----------------------------------------------
    def _stats(self) -> dict:
        if self.driver == "psycopg":
            return {"stats": pg_stats(self.pool), "private": pg_private(self.pool)}
        return apg_stats(self.pool)

    async def _pid(self, conn):
        return safe_pg_pid(conn) if self.driver == "psycopg" else safe_apg_pid(conn)

    async def _acquire(self):
        if self.driver == "psycopg":
            res = await bound(self.pool.getconn(timeout=BOUND), BOUND)
        else:
            res = await bound(self.pool.acquire(timeout=BOUND), BOUND)
        if res[0] != "ok":
            raise RuntimeError(f"acquire failed: {res!r}")
        return res[1]

    async def _release(self, conn) -> tuple:
        if self.driver == "psycopg":
            return await bound(self.pool.putconn(conn), BOUND)
        return await bound(self.pool.release(conn), BOUND)

    async def _exec(self, conn, sql: str):
        if self.driver == "psycopg":
            cur = await conn.execute(sql)
            return cur.statusmessage
        return await conn.execute(sql)

    async def _fetchval(self, conn, sql: str):
        if self.driver == "psycopg":
            cur = await conn.execute(sql)
            row = await cur.fetchone()
            return None if row is None else row[0]
        return await conn.fetchval(sql)

    async def _legit_ok(self) -> dict:
        if self.legit is None:
            return {"alive": False, "error": "legit handle was None"}
        try:
            guc = await self._fetchval(self.legit, "SELECT current_setting('dw.legit')")
            temp = await self._fetchval(self.legit, "SELECT x FROM dw_legit_tmp")
            pid = await self._pid(self.legit)
            return {"alive": True, "pid": pid, "same_pid": pid == self.legit_pid,
                    "guc": guc, "temp": temp}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"alive": False, "error": err_text(exc)}

    async def _sample(self) -> dict:
        return {
            "stats": self._stats(),
            "server": await self.observer.sample(self.app),
            "legit": await self._legit_ok(),
        }

    async def _settle(self, want_available: int | None = None,
                      timeout: float = 2.0) -> dict:
        started = time.monotonic()
        while time.monotonic() - started < timeout:
            snap = await self._sample()
            stats = snap["stats"]
            if self.driver == "psycopg":
                available = stats["stats"].get("pool_available")
                waiting = stats["stats"].get("requests_waiting")
                ready = waiting == 0 and (
                    want_available is None or available == want_available
                )
                if ready:
                    return snap
                await asyncio.sleep(0.01)
            else:
                # asyncpg's _queue holds idle holders, not waiters: qsize is
                # not a waiting count.  Use a short fixed settle and the idle
                # count; a waiter is represented by an acquire timeout, not
                # by qsize.
                available = stats.get("idle")
                if want_available is None:
                    await asyncio.sleep(0.05)
                    return await self._sample()
                if available == want_available:
                    return snap
                await asyncio.sleep(0.01)
        return await self._sample()

    # ---- shapes ------------------------------------------------------
    async def target_delivery(self) -> None:
        self.block_check = True
        self.check_entered.clear()
        self.check_release.clear()
        task = asyncio.create_task(self._acquire())
        try:
            await asyncio.wait_for(self.check_entered.wait(), BOUND)
            self.rec["at_delivery"] = await self._sample()
            self.rec["at_delivery"]["delivered_pid"] = self.delivered_pid
        finally:
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                self.rec["cut_realized"] = True
            except BaseException as exc:
                self.rec["cancel_error"] = err_text(exc)
            finally:
                self.block_check = False
                self.check_release.set()

    async def target_early(self) -> None:
        task = asyncio.create_task(self._acquire())
        await asyncio.sleep(0)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            self.rec["cut_realized"] = True
        except BaseException as exc:
            self.rec["cancel_error"] = err_text(exc)
        self.delivered_pid = None

    async def target_late(self) -> None:
        if self.driver == "psycopg":
            async def use():
                async with self.pool.connection() as conn:
                    self.delivered_pid = safe_pg_pid(conn)
                    self.rec["delivered_pid"] = self.delivered_pid
                    await conn.execute("SELECT pg_sleep(0.4)")
        else:
            async def use():
                async with self.pool.acquire() as conn:
                    self.delivered_pid = safe_apg_pid(conn)
                    self.rec["delivered_pid"] = self.delivered_pid
                    await conn.execute("SELECT pg_sleep(0.4)")
        tr = await _await_target(use(), 2, BOUND)
        self.rec["target"] = {
            "outcome": tr.outcome, "realized": tr.realized,
            "yields": tr.yields, "error": tr.error,
        }
        self.rec["cut_realized"] = tr.realized

    async def target_normal(self) -> None:
        conn = await self._acquire()
        self.delivered_pid = await self._pid(conn)
        self.rec["delivered_pid"] = self.delivered_pid
        self.rec["query"] = await self._fetchval(conn, "SELECT 1")
        self.rec["release"] = list(await self._release(conn))

    async def _next_acquire(self) -> dict:
        try:
            conn = await self._acquire()
            pid = await self._pid(conn)
            value = await self._fetchval(conn, "SELECT 1")
            rel = await self._release(conn)
            return {"ok": True, "pid": pid, "same_as_delivered": pid == self.delivered_pid,
                    "query": value, "release": list(rel)}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"ok": False, "error": err_text(exc)}

    async def run(self) -> dict:
        started = time.monotonic()
        try:
            await asyncio.wait_for(self.setup(), 15)
            self.rec["pre"] = await self._sample()
            if self.shape == "delivery":
                await self.target_delivery()
            elif self.shape == "early":
                await self.target_early()
            elif self.shape == "late":
                await self.target_late()
            elif self.shape == "normal":
                await self.target_normal()
            else:
                raise RuntimeError(f"unknown shape {self.shape}")
            self.rec["after_cut"] = await self._settle()
            self.rec["after_settle"] = await self._settle()
            self.rec["next"] = await self._next_acquire()
            self.rec["after_next"] = await self._settle()
            # Release the legitimate user last, then check capacity recovers.
            if self.legit is not None:
                self.rec["legit_release"] = list(await self._release(self.legit))
                self.legit = None
            self.rec["final"] = await self._settle(want_available=2)
            self.rec["final_legit_state"] = self.rec.get("final", {}).get("legit")
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            self.rec["error"] = err_text(exc)
            self.rec["trace"] = __import__("traceback").format_exc()[-1600:]
        finally:
            await self.cleanup()
            self.rec["seconds"] = round(time.monotonic() - started, 3)
        return self.rec

    async def cleanup(self) -> None:
        if self.legit is not None:
            with contextlib.suppress(BaseException):
                await self._release(self.legit)
            self.legit = None
        if self.pool is not None:
            pool, self.pool = self.pool, None
            if self.driver == "psycopg":
                with contextlib.suppress(BaseException):
                    await pool.close(timeout=2.0)
            else:
                with contextlib.suppress(BaseException):
                    await pool.close()
        with contextlib.suppress(BaseException):
            await self.observer.close()


def evaluate(rec: dict) -> dict:
    issues: list[str] = []
    if rec.get("error"):
        issues.append("trial_error")
    if rec.get("shape") == "delivery" and not rec.get("cut_realized"):
        issues.append("delivery_cut_not_realized")
    after = rec.get("after_settle") or {}
    nxt = rec.get("next") or {}
    final = rec.get("final") or {}
    legit = (after.get("legit") or {})
    if after:
        if not legit.get("alive"):
            issues.append("legit_user_lost")
        elif not legit.get("same_pid"):
            issues.append("legit_user_pid_changed")
        elif legit.get("guc") != "keeper" or legit.get("temp") != 7:
            issues.append("legit_session_state_lost")
        if not nxt.get("ok"):
            issues.append("next_acquire_failed")
        elif nxt.get("query") != 1:
            issues.append("next_query_failed")
    if final:
        stats = final.get("stats") or {}
        if rec.get("driver") == "psycopg":
            st = stats.get("stats") or {}
            if st.get("pool_size") != 2:
                issues.append(f"final_pool_size_{st.get('pool_size')}")
            if st.get("pool_available") != 2:
                issues.append(f"final_available_{st.get('pool_available')}")
            if st.get("requests_waiting") != 0:
                issues.append(f"final_waiting_{st.get('requests_waiting')}")
        else:
            if stats.get("size") != 2:
                issues.append(f"final_size_{stats.get('size')}")
            if stats.get("idle") != 2:
                issues.append(f"final_idle_{stats.get('idle')}")
        server = final.get("server") or {}
        if server.get("count") != 2:
            issues.append(f"final_server_count_{server.get('count')}")
    return {"issues": issues, "clean": not issues}


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--user", default="postgres")
    ap.add_argument("--password", default="dw")
    ap.add_argument("--dbname", default="dw")
    ap.add_argument("--driver", choices=["psycopg", "asyncpg"], required=True)
    ap.add_argument("--shapes", default="normal,early,delivery,late")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for shape in args.shapes.split(","):
                sc = DeliveryScenario(args, shape, rep)
                try:
                    rec = await asyncio.wait_for(sc.run(), 40)
                except asyncio.CancelledError:
                    raise
                except BaseException as exc:
                    failures += 1
                    rec = {"driver": args.driver, "shape": shape, "rep": rep,
                           "error": err_text(exc),
                           "trace": __import__("traceback").format_exc()[-1600:]}
                rec["evaluation"] = evaluate(rec)
                if not rec["evaluation"]["clean"]:
                    failures += 1
                print("DELIVERY " + json.dumps(rec, default=str, sort_keys=True),
                      file=out, flush=True)
                print(
                    f"DELIVERY-SUMMARY driver={args.driver} shape={shape} rep={rep} "
                    f"cut={rec.get('cut_realized')} "
                    f"next_ok={((rec.get('next') or {}).get('ok'))} "
                    f"next_same={((rec.get('next') or {}).get('same_as_delivered'))} "
                    f"issues={rec['evaluation']['issues']} "
                    f"seconds={rec.get('seconds')}",
                    flush=True,
                )
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
