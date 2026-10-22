"""asyncpg: what can a caller do with the *same lease* after the entry fix?

The round-4 asyncpg repair terminates the connection from a cancelled
``Transaction.__aenter__``.  This module tests the boundary of that fix rather
than calling it a fake repair:

1. delay the BEGIN reply through a real proxy, so the server has executed
   BEGIN while the caller is suspended at transaction entry;
2. cancel at that suspension and catch the cancellation without returning or
   resetting the connection;
3. run an ordinary autocommit ``INSERT`` on the same connection and record
   exactly what happens;
4. read the table from an independent direct connection;
5. return the handle to a one-slot pool, wait for its reset, re-read the table
   and borrow again.

Comparisons: no cancellation (normal), cancellation + explicit ``reset()``,
cancellation + retry through the transaction API, and a body cancellation.
The original package and the local fail-closed patch are run separately.

Outcomes are classified as committed success, explicit refusal, uncommitted
success, or rolled back on return.  ``_top_xact = None`` is never treated as
proof that the server is IDLE; the independent observer and the pool state are
recorded separately.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import sys
import time

import asyncpg

from cases.common import (
    BOUND,
    DelayProxy,
    add_db_args,
    bound,
    close_quiet,
    err_text,
    unique_table,
)
from cases.txphase_case import _await_phased, install_asyncpg_enter_fix


class LeaseScenario:
    def __init__(self, args, mode: str, rep: int) -> None:
        self.args = args
        self.mode = mode
        self.rep = rep
        self.table = unique_table("dw_postfix")
        self.app = f"dw-postfix-{os.getpid()}-{rep}-{mode}"
        self.obs_app = f"{self.app}-obs"
        self.proxy: DelayProxy | None = None
        self.proxy_port: int | None = None
        self.observer: asyncpg.Connection | None = None
        self.pool = None
        self.conn = None
        self.pid = None
        self.tx = None
        self.holder: dict = {"phase": "setup"}
        self.rec: dict = {
            "mode": mode, "rep": rep, "driver": "asyncpg",
            "table": self.table, "app": self.app,
        }
        self.times: dict[str, float] = {}

    def _dsn(self, port: int, app: str | None = None) -> str:
        a = self.args
        return (
            f"postgresql://{a.user}:{a.password}@{a.host}:{port}/{a.dbname}"
            f"?application_name={app or self.app}"
        )

    async def setup(self) -> None:
        self.proxy = DelayProxy(self.args.host, self.args.port, delay=self.args.delay)
        self.proxy_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        self.observer = await asyncpg.connect(
            self._dsn(self.args.port, self.obs_app), timeout=BOUND
        )
        await self.observer.execute(
            f"CREATE TABLE {self.table} (k text PRIMARY KEY, v integer NOT NULL)"
        )
        if self.args.direct:
            self.conn = await asyncpg.connect(
                self._dsn(self.proxy_port), timeout=BOUND
            )
        else:
            self.pool = await asyncpg.create_pool(
                self._dsn(self.proxy_port), min_size=1, max_size=1,
                timeout=BOUND,
            )
            self.conn = await self.pool.acquire()
        self.pid = self.conn.get_server_pid()
        self.rec["setup"] = {
            "pid": self.pid, "proxy_port": self.proxy_port,
            "server": await self._server(),
        }

    def _state(self) -> dict:
        conn = self.conn
        if conn is None:
            return {"closed": None, "in_transaction": None}
        try:
            closed = bool(conn.is_closed())
        except BaseException:
            closed = None
        try:
            in_tx = None if closed else bool(conn.is_in_transaction())
        except BaseException:
            in_tx = None
        try:
            top = type(getattr(conn, "_top_xact", None)).__name__
        except BaseException:
            top = None
        try:
            tx_state = getattr(getattr(conn, "_top_xact", None), "_state", None)
            tx_state = getattr(tx_state, "name", str(tx_state))
        except BaseException:
            tx_state = None
        return {
            "closed": closed, "in_transaction": in_tx,
            "top_xact": top, "top_xact_state": tx_state,
        }

    async def _server(self) -> dict:
        assert self.observer is not None
        try:
            row = await self.observer.fetchrow(
                "SELECT pid, state, left(query, 120) AS query, "
                "       backend_xid::text AS xid, xact_start::text AS xact_start "
                "FROM pg_stat_activity WHERE application_name = $1 "
                "AND pid <> pg_backend_pid()",
                self.app,
            )
            if row is None:
                return {"present": False}
            return {"present": True, "pid": row["pid"], "state": row["state"],
                    "query": row["query"], "xid": row["xid"],
                    "xact_start": row["xact_start"]}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def _rows(self) -> list[list]:
        assert self.observer is not None
        rows = await self.observer.fetch(
            f"SELECT k, v FROM {self.table} ORDER BY k"
        )
        return [[r["k"], r["v"]] for r in rows]

    async def _insert(self, key: str, value: int = 1) -> dict:
        t0 = time.monotonic()
        try:
            call = self.conn.execute(
                f"INSERT INTO {self.table} (k, v) VALUES ($1, $2)", key, value
            )
        except BaseException as exc:
            # A PoolConnectionProxy whose underlying connection was terminated
            # raises synchronously on attribute access.  That is an explicit
            # refusal, not a harness error.
            res = ("err", err_text(exc))
        else:
            res = await bound(call, self.args.timeout)
        return {
            "result": list(res),
            "seconds": round(time.monotonic() - t0, 4),
            "state": self._state(),
            "rows": await self._rows(),
            "server": await self._server(),
        }

    async def _entry_cancel(self) -> dict:
        """Cancel only after the observer can see BEGIN executed.

        The proxy delays the BEGIN reply.  Polling the independent observer is
        what makes "the server executed BEGIN" an observation instead of an
        assumption about the driver's send order.
        """
        self.tx = self.conn.transaction()
        self.holder["phase"] = "enter"
        started = time.monotonic()
        task = asyncio.create_task(self.tx.__aenter__())
        begin_observed = None
        deadline = time.monotonic() + self.args.timeout
        while time.monotonic() < deadline:
            srv = await self._server()
            if srv.get("present") and srv.get("xact_start"):
                begin_observed = srv
                break
            if task.done():
                break
            await asyncio.sleep(0.01)
        if begin_observed is None:
            if not task.done():
                task.cancel()
            with contextlib.suppress(BaseException):
                await task
            return {
                "outcome": "unrealized",
                "realized": False,
                "error": "BEGIN did not become visible before the cut",
                "seconds": round(time.monotonic() - started, 4),
                "phase": "enter",
                "client": self._state(),
                "server": await self._server(),
            }
        task.cancel()
        err = None
        try:
            await task
        except asyncio.CancelledError:
            pass
        except BaseException as exc:
            err = err_text(exc)
        await asyncio.sleep(0.05)
        return {
            "outcome": "cancelled" if err is None else "error",
            "realized": True,
            "error": err,
            "seconds": round(time.monotonic() - started, 4),
            "phase": "enter",
            "begin_observed": begin_observed,
            "client": self._state(),
            "server": await self._server(),
        }

    async def _body_cancel(self) -> dict:
        async def body():
            self.holder["phase"] = "enter"
            async with self.conn.transaction():
                self.holder["phase"] = "body"
                await self.conn.execute(
                    f"INSERT INTO {self.table} (k, v) VALUES ($1, $2)",
                    "target", 1,
                )
                self.holder["phase"] = "body_wait"
                await asyncio.sleep(0.5)

        started = time.monotonic()
        tr, meta = await _await_phased(
            body(), cut_after=2, timeout=self.args.timeout,
            holder=self.holder, double=False,
        )
        return {
            "outcome": tr.outcome, "realized": tr.realized,
            "yields": tr.yields, "error": tr.error,
            "seconds": round(time.monotonic() - started, 4),
            "phase": meta["cut_phase"],
            "client": self._state(),
            "server": await self._server(),
        }

    async def run(self) -> dict:
        started = time.monotonic()
        try:
            await asyncio.wait_for(self.setup(), 20)
            self.rec["before"] = {
                "client": self._state(), "server": await self._server(),
                "rows": await self._rows(),
            }
            if self.mode == "normal":
                self.rec["normal"] = await self._normal()
            elif self.mode == "entry_same_lease":
                self.rec["cut"] = await self._entry_cancel()
                self.rec["after_cut"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
                self.rec["same_lease"] = await self._insert("same_lease")
                self.rec["after_same_lease"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
            elif self.mode == "entry_reset":
                self.rec["cut"] = await self._entry_cancel()
                self.rec["after_cut"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
                try:
                    reset_call = self.conn.reset()
                except BaseException as exc:
                    self.rec["reset"] = ["err", err_text(exc)]
                else:
                    self.rec["reset"] = list(await bound(
                        reset_call, self.args.timeout
                    ))
                self.rec["after_reset"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
                self.rec["same_lease"] = await self._insert("after_reset")
            elif self.mode == "entry_txapi":
                self.rec["cut"] = await self._entry_cancel()
                self.rec["after_cut"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
                try:
                    async with self.conn.transaction():
                        try:
                            txwrite = self.conn.execute(
                                f"INSERT INTO {self.table} (k, v) "
                                "VALUES ($1, $2)", "txapi", 1,
                            )
                        except BaseException as exc:
                            self.rec["txapi_write"] = ["err", err_text(exc)]
                        else:
                            self.rec["txapi_write"] = await bound(
                                txwrite, self.args.timeout
                            )
                    self.rec["txapi_exit"] = "ok"
                except asyncio.CancelledError:
                    raise
                except BaseException as exc:
                    self.rec["txapi_exit"] = err_text(exc)
                self.rec["after_txapi"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
            elif self.mode == "body_cancel":
                self.rec["cut"] = await self._body_cancel()
                self.rec["after_cut"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
                self.rec["same_lease"] = await self._insert("after_body")
            else:
                raise RuntimeError(f"unknown mode {self.mode}")

            if self.args.direct:
                self.rec["direct_close"] = "no pool in direct mode"
                self.rec["direct_after"] = {
                    "client": self._state(), "server": await self._server(),
                    "rows": await self._rows(),
                }
                return self.rec
            # Step 5: return the same lease to the pool, wait for its reset,
            # then borrow again and perform a visible write.
            self.times["before_return"] = time.monotonic() - started
            ret_started = time.monotonic()
            try:
                release_call = self.pool.release(self.conn)
            except BaseException as exc:
                release = ("err", err_text(exc))
            else:
                release = await bound(release_call, self.args.timeout)
            self.conn = None
            self.rec["release"] = list(release)
            self.rec["release_seconds"] = round(time.monotonic() - ret_started, 4)
            # Wait for reset/maintenance to settle without assuming a duration.
            await asyncio.sleep(self.args.settle)
            self.rec["after_return"] = {
                "server": await self._server(), "rows": await self._rows(),
                "stats": self._stats(),
            }
            next_started = time.monotonic()
            self.conn = await self.pool.acquire()
            self.rec["next"] = {
                "seconds": round(time.monotonic() - next_started, 4),
                "pid": self.conn.get_server_pid(),
                "same_pid": self.conn.get_server_pid() == self.pid,
                "client": self._state(),
            }
            self.rec["next_write"] = await self._insert("after_borrow", 9)
            self.rec["after_next_write"] = {
                "server": await self._server(), "rows": await self._rows(),
            }
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            self.rec["error"] = err_text(exc)
            self.rec["trace"] = __import__("traceback").format_exc()[-1600:]
        finally:
            await self.cleanup()
            self.rec["seconds"] = round(time.monotonic() - started, 3)
        return self.rec

    async def _normal(self) -> dict:
        async with self.conn.transaction():
            res = await self.conn.execute(
                f"INSERT INTO {self.table} (k, v) VALUES ($1, $2)", "normal", 1
            )
        return {"write": res, "rows": await self._rows(),
                "client": self._state(), "server": await self._server()}

    def _stats(self) -> dict:
        if self.pool is None:
            return {}
        try:
            return {"size": self.pool.get_size(),
                    "idle": self.pool.get_idle_size()}
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def cleanup(self) -> None:
        if self.conn is not None:
            if self.pool is not None:
                with contextlib.suppress(BaseException):
                    await self.pool.release(self.conn)
            else:
                await close_quiet(self.conn.close())
            self.conn = None
        if self.pool is not None:
            with contextlib.suppress(BaseException):
                await self.pool.close()
            self.pool = None
        if self.observer is not None:
            with contextlib.suppress(BaseException):
                await self.observer.execute(
                    f"DROP TABLE IF EXISTS {self.table}"
                )
            await close_quiet(self.observer.close())
            self.observer = None
        if self.proxy is not None:
            await close_quiet(self.proxy.close())
            self.proxy = None


def classify(rec: dict) -> dict:
    """Separate success/refusal/uncommitted/rollback for the same lease."""
    same = rec.get("same_lease") or {}
    after = rec.get("after_same_lease") or {}
    ret = rec.get("after_return") or {}
    nextw = rec.get("next_write") or {}
    labels = []
    if rec.get("error"):
        labels.append("trial_error")
    result = (same.get("result") or [None])[0]
    rows = same.get("rows") or []
    if result == "ok":
        if rows:
            labels.append("committed_success")
        else:
            labels.append("uncommitted_success")
    elif result in ("err", "timeout"):
        labels.append("explicit_refusal")
    elif rec.get("mode") == "entry_txapi":
        if (rec.get("txapi_write") or [None])[0] == "ok" and not (rec.get("after_txapi") or {}).get("rows"):
            labels.append("txapi_success_invisible")
        elif rec.get("txapi_write"):
            labels.append("txapi_refusal")
        elif rec.get("txapi_exit") and rec.get("txapi_exit") != "ok":
            labels.append("txapi_refusal")
    if rec.get("mode") == "entry_txapi":
        if (rec.get("after_txapi") or {}).get("rows"):
            labels.append("txapi_visible")
    if ret.get("rows") is not None:
        labels.extend([f"after_return_rows_{len(ret['rows'])}"])
    if (nextw.get("rows") or []) and ["after_borrow", 9] in nextw["rows"]:
        labels.append("next_borrow_visible")
    return {"labels": labels,
            "same_lease_result": result,
            "same_lease_visible": bool(rows),
            "after_return_rows": ret.get("rows"),
            "next_borrow_visible": ["after_borrow", 9] in (nextw.get("rows") or [])}


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--modes", default="normal,entry_same_lease,entry_reset,entry_txapi,body_cancel")
    ap.add_argument("--delay", type=float, default=0.05)
    ap.add_argument("--settle", type=float, default=0.2)
    ap.add_argument("--fixed", action="store_true")
    ap.add_argument("--direct", action="store_true",
                    help="use a direct tested connection, not a one-slot pool")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    if args.fixed:
        install_asyncpg_enter_fix()
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for mode in args.modes.split(","):
                sc = LeaseScenario(args, mode.strip(), rep)
                try:
                    rec = await asyncio.wait_for(sc.run(), 45)
                except asyncio.CancelledError:
                    raise
                except BaseException as exc:
                    failures += 1
                    rec = {"mode": mode, "rep": rep, "trial_error": err_text(exc),
                           "trace": __import__("traceback").format_exc()[-1600:]}
                rec["classification"] = classify(rec)
                if rec.get("error") or rec.get("trial_error"):
                    failures += 1
                print("LEASE " + json.dumps(rec, default=str, sort_keys=True),
                      file=out, flush=True)
                print(
                    f"LEASE-SUMMARY fixed={args.fixed} mode={mode} rep={rep} "
                    f"cut={((rec.get('cut') or {}).get('realized'))} "
                    f"phase={((rec.get('cut') or {}).get('phase'))} "
                    f"server_after_cut={((rec.get('cut') or {}).get('server'))} "
                    f"same={((rec.get('same_lease') or {}).get('result'))} "
                    f"labels={rec['classification']['labels']} "
                    f"release_s={rec.get('release_seconds')} "
                    f"next_s={((rec.get('next') or {}).get('seconds'))} "
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
