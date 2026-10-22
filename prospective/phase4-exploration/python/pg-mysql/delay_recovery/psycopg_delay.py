"""psycopg: delayed BEGIN/SAVEPOINT replies, recovery and re-cancellation.

Reuses the round-4 CmScenario (connection, proxy, observer, official
transaction CM) but changes only the proxy delay *after* setup.  The first cut
is requested only after an independent observer sees the tested backend in a
transaction, so a 5 s reply delay is a real in-flight reply, not a guess.

Measured separately:
  (a) cancel request -> caller observes cancellation;
  (b) cancel request -> recovery action completed (or connection closed);
  (c) cancel request -> a fresh successor connection completes a visible write.

Modes cover original rollback, explicit close, reconnect, the local fail-closed
entry fix, no recovery, and a second cancel during recovery.  Nested mode keeps
an explicit outer transaction open while the inner (SAVEPOINT) entry is cut.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import sys
import time

import psycopg

from cases.common import add_db_args, bound, close_quiet, err_text, must_ok
from cases.txphase_case import CmScenario, install_psycopg_enter_fix, _await_phased


async def _timed(aw, timeout: float):
    return await asyncio.wait_for(aw, timeout)


class OneWayDelayProxy:
    """Loopback proxy that delays only server->client response chunks."""

    def __init__(self, host: str, port: int, delay: float = 0.05) -> None:
        self.host = host
        self.port = port
        self.delay = delay
        self._server = None
        self._tasks: set[asyncio.Task] = set()
        self._writers: set = set()

    async def start(self) -> int:
        self._server = await asyncio.start_server(self._handle, "127.0.0.1", 0)
        assert self._server.sockets
        return self._server.sockets[0].getsockname()[1]

    async def _handle(self, reader, writer) -> None:
        self._writers.add(writer)
        try:
            up_reader, up_writer = await asyncio.open_connection(self.host, self.port)
        except BaseException:
            writer.close(); self._writers.discard(writer); return
        self._writers.add(up_writer)
        task = asyncio.current_task(); assert task is not None
        self._tasks.add(task)
        try:
            await asyncio.gather(
                self._pipe(reader, up_writer, delay=False),
                self._pipe(up_reader, writer, delay=True),
            )
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            self._tasks.discard(task)
            for w in (writer, up_writer):
                with contextlib.suppress(BaseException):
                    w.close()
                self._writers.discard(w)

    async def _pipe(self, reader, writer, *, delay: bool) -> None:
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                if delay and self.delay:
                    await asyncio.sleep(self.delay)
                writer.write(data)
                await writer.drain()
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            with contextlib.suppress(BaseException):
                writer.close()

    async def close(self) -> None:
        if self._server is not None:
            self._server.close()
            with contextlib.suppress(BaseException):
                await self._server.wait_closed()
            self._server = None
        for task in list(self._tasks):
            task.cancel()
        for task in list(self._tasks):
            with contextlib.suppress(BaseException):
                await task
        for writer in list(self._writers):
            with contextlib.suppress(BaseException):
                writer.close()
        self._tasks.clear(); self._writers.clear()


def _client_state(sc: CmScenario) -> dict:
    st = sc._client_state()
    try:
        st["lock_locked"] = bool(sc.conn.lock.locked()) if sc.conn else None
    except BaseException:
        st["lock_locked"] = None
    try:
        st["tx_status"] = str(sc.conn.info.transaction_status) if sc.conn else None
    except BaseException:
        st["tx_status"] = None
    return st


class DelayRecovery:
    def __init__(self, args, mode: str, nested: bool, double: bool, rep: int):
        self.args = args
        self.mode = mode
        self.nested = nested
        self.double = double
        self.rep = rep
        self.sc = CmScenario(args)
        self.rec: dict = {
            "mode": mode, "nested": nested, "double": double, "rep": rep,
            "delay": args.delay, "driver": "psycopg",
            "table": self.sc.table,
        }

    async def setup(self) -> None:
        if self.args.oneway:
            # Reuse CmScenario's connection helpers, but a one-way proxy: only
            # the BEGIN/SAVEPOINT response is delayed, not the client request.
            self.proxy = OneWayDelayProxy(
                self.args.host, self.args.port, delay=0.05
            )
            self.sc.proxy = self.proxy
            self.sc.proxy_port = await _timed(self.proxy.start(), 10)
            self.sc.observer = must_ok(
                "observer connect", await bound(self.sc._observer_connect())
            )
            created = await bound(self.sc.observer.execute(
                f"CREATE TABLE {self.sc.table} "
                "(k text PRIMARY KEY, v integer NOT NULL)"
            ))
            must_ok("create table", created)
            self.sc.conn = must_ok(
                "tested connect",
                await bound(self.sc._tested_connect(self.sc.proxy_port)),
            )
            self.sc.pid = await self.sc._conn_pid()
            normal = await self.sc._cm_write("preflight", 1)
            if not normal.ok:
                raise RuntimeError(f"preflight failed: {normal!r}")
            rows = await self.sc._observer_rows()
            if rows != [["preflight", 1]]:
                raise RuntimeError(f"preflight not observed: {rows!r}")
            deleted = await bound(
                self.sc.observer.execute(f"DELETE FROM {self.sc.table}")
            )
            must_ok("preflight delete", deleted)
            self.proxy.delay = self.args.delay
        else:
            await _timed(self.sc.setup(), 15)
            # Setup itself used the normal 50 ms proxy.  Only the target/recovery
            # window gets the long delay.
            if self.sc.proxy is not None:
                self.sc.proxy.delay = self.args.delay
        if self.mode == "fix":
            install_psycopg_enter_fix()

    async def _begin_visible(self, timeout: float) -> dict | None:
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = await self.sc._server_sample()
            if os.environ.get("DW_DEBUG"):
                print("DBG", round(time.monotonic() - deadline, 3), last, "pid", self.sc.pid, flush=True)
            if (
                "in transaction" in (last.get("state") or "")
                or last.get("in_xact") == "true"
            ):
                return last
            await asyncio.sleep(0.005)
        return None

    async def _entry_cut(self, *, inner: bool) -> dict:
        sc = self.sc
        tx = sc.conn.transaction()
        sc.tx = tx
        sc.holder["phase"] = "inner_enter" if inner else "enter"
        started = time.monotonic()
        task = asyncio.create_task(tx.__aenter__())
        begin = await self._begin_visible(self.args.timeout)
        # For an inner savepoint the backend is already INTRANS; BEGIN
        # visibility is not an inner proof, so record both facts.
        if begin is None and not inner:
            if not task.done():
                task.cancel()
            with contextlib.suppress(BaseException):
                await task
            return {"realized": False, "error": "BEGIN never visible",
                    "phase": sc.holder["phase"], "seconds": round(time.monotonic() - started, 3)}
        # Give the driver a moment to actually suspend on the delayed reply.
        await asyncio.sleep(0.01)
        t_cut = time.monotonic()
        task.cancel()
        second_task = None
        second = None
        if self.double:
            async def second_cancel():
                await asyncio.sleep(self.args.second_delay)
                if not task.done():
                    second["time"] = time.monotonic()
                    second["phase"] = sc.holder.get("phase")
                    task.cancel()
                else:
                    second["time"] = None
            second = {}
            second_task = asyncio.create_task(second_cancel())
        err = None
        try:
            await task
        except asyncio.CancelledError:
            pass
        except BaseException as exc:
            err = err_text(exc)
        t_return = time.monotonic()
        if second_task is not None:
            second_task.cancel()
            with contextlib.suppress(BaseException):
                await second_task
        return {
            "realized": True,
            "begin_observed": begin,
            "inner": inner,
            "phase": sc.holder.get("phase"),
            "t_cut": t_cut,
            "t_cancel_return": t_return,
            "cancel_to_return": round(t_return - t_cut, 4),
            "total_seconds": round(t_return - started, 4),
            "error": err,
            "second": second,
            "client": _client_state(sc),
            "server": await sc._server_sample(),
        }

    async def _recover(self, cut: dict) -> dict:
        sc = self.sc
        out: dict = {"mode": self.mode}
        t0 = time.monotonic()
        if self.mode == "fix":
            # The fix runs inside the cancelled __aenter__; its duration is
            # already in cancel_to_return.  Still inspect the result.
            out["kind"] = "inside_cancel"
            out["ok"] = cut.get("error") is None
            out["lock_locked"] = _client_state(sc).get("lock_locked")
        elif self.mode == "close":
            out["kind"] = "close"
            try:
                await _timed(sc.conn.close(), self.args.timeout)
                out["ok"] = True
            except BaseException as exc:
                out["ok"] = False
                out["error"] = err_text(exc)
        elif self.mode == "none":
            out["kind"] = "none"
            out["ok"] = True
        else:
            out["kind"] = "rollback" if self.mode == "rollback" else "reconnect"
            sc.recovery = out["kind"]
            rec_task = asyncio.create_task(sc._recover())
            second = None
            second_task = None
            if self.double:
                second = {}
                async def recancel():
                    await asyncio.sleep(self.args.second_delay)
                    if not rec_task.done():
                        second["time"] = time.monotonic()
                        second["phase"] = "recovery"
                        rec_task.cancel()
                second_task = asyncio.create_task(recancel())
            try:
                out["result"] = await _timed(rec_task, self.args.timeout)
                if isinstance(out["result"], dict):
                    res = out["result"].get("result")
                    if isinstance(res, (list, tuple)) and res and res[0] == "err":
                        out["ok"] = False
                        out["error"] = res[1]
                    else:
                        out["ok"] = True
                else:
                    out["ok"] = True
            except asyncio.CancelledError:
                out["ok"] = False
                out["error"] = "recovery task cancelled"
            except BaseException as exc:
                out["ok"] = False
                out["error"] = err_text(exc)
            if second_task is not None:
                second_task.cancel()
                with contextlib.suppress(BaseException):
                    await second_task
            out["second"] = second
        out["seconds"] = round(time.monotonic() - t0, 4)
        out["client"] = _client_state(sc)
        out["server"] = await sc._server_sample()
        return out

    async def _watch_server(self, cut: dict) -> dict:
        """Measure old-resource release separately from successor availability."""
        t0 = time.monotonic()
        samples = 0
        last = None
        while time.monotonic() - t0 < self.args.old_cleanup_wait:
            last = await self.sc._server_sample()
            samples += 1
            if last.get("present") is False or "state" not in last:
                return {"cleared": True, "seconds": round(time.monotonic() - t0, 4),
                        "samples": samples, "last": last,
                        "cancel_to_cleared": round(time.monotonic() - cut["t_cut"], 4)}
            if last.get("in_xact") == "false" and last.get("state") != "idle in transaction":
                return {"cleared": True, "seconds": round(time.monotonic() - t0, 4),
                        "samples": samples, "last": last,
                        "cancel_to_cleared": round(time.monotonic() - cut["t_cut"], 4)}
            await asyncio.sleep(0.02)
        return {"cleared": False, "seconds": round(time.monotonic() - t0, 4),
                "samples": samples, "last": last,
                "cancel_to_cleared": None}

    async def _successor(self, cut: dict) -> dict:
        sc = self.sc
        t0 = time.monotonic()
        # A fresh handle and a fresh schema-visible write; not the failed handle.
        try:
            conn = await _timed(
                psycopg.AsyncConnection.connect(sc._dsn(sc.args.port), autocommit=True),
                self.args.timeout,
            )
            try:
                cur = await _timed(
                    conn.execute(
                        f"INSERT INTO {sc.table} (k, v) VALUES (%s, %s) "
                        "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
                        ("successor", 7),
                    ),
                    self.args.timeout,
                )
                res = cur.statusmessage
            finally:
                await close_quiet(conn.close())
        except BaseException as exc:
            return {"ok": False, "error": err_text(exc),
                    "cancel_to_successor": round(time.monotonic() - cut["t_cut"], 4),
                    "seconds": round(time.monotonic() - t0, 4)}
        rows = await _timed(sc._observer_rows(), self.args.timeout)
        return {
            "ok": True, "result": res, "rows": rows,
            "successor_visible": ["successor", 7] in rows,
            "cancel_to_successor": round(time.monotonic() - cut["t_cut"], 4),
            "seconds": round(time.monotonic() - t0, 4),
        }

    async def run(self) -> dict:
        started = time.monotonic()
        try:
            await self.setup()
            self.rec["setup"] = {"pid": self.sc.pid, "proxy_port": self.sc.proxy_port}
            if self.nested:
                await self._run_nested()
            else:
                cut = await self._entry_cut(inner=False)
                self.rec["cut"] = cut
                if cut.get("realized"):
                    self.rec["recovery"] = await self._recover(cut)
                    self.rec["successor"] = await self._successor(cut)
                    self.rec["old_cleanup"] = await self._watch_server(cut)
            self.rec["total_seconds"] = round(time.monotonic() - started, 3)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            self.rec["error"] = err_text(exc)
            self.rec["trace"] = __import__("traceback").format_exc()[-1600:]
        finally:
            await close_quiet(self.sc.close())
        return self.rec

    async def _run_nested(self) -> None:
        sc = self.sc
        outer = sc.conn.transaction()
        await _timed(outer.__aenter__(), self.args.timeout)
        cur = await _timed(
            sc.conn.execute(
                f"INSERT INTO {sc.table} (k, v) VALUES (%s, %s) "
                "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
                ("outer_before", 1),
            ),
            self.args.timeout,
        )
        self.rec["outer"] = {
            "write": cur.statusmessage,
            "client": _client_state(sc),
            "server": await sc._server_sample(),
        }
        cut = await self._entry_cut(inner=True)
        self.rec["cut"] = cut
        if not cut.get("realized"):
            return
        self.rec["recovery"] = await self._recover(cut)
        # End the legitimate outer transaction explicitly.  A rollback checks
        # that the outer write does not leak; the successor then checks that the
        # handle/connection is usable.
        try:
            await _timed(outer.__aexit__(Exception, Exception("test_end"), None), self.args.timeout)
            self.rec["outer_exit"] = "rollback_ok"
        except BaseException as exc:
            self.rec["outer_exit"] = err_text(exc)
            with contextlib.suppress(BaseException):
                await _timed(sc.conn.rollback(), self.args.timeout)
        self.rec["after_outer"] = {
            "client": _client_state(sc), "server": await sc._server_sample(),
            "rows": await _timed(sc._observer_rows(), self.args.timeout),
        }
        self.rec["successor"] = await self._successor(cut)
        self.rec["old_cleanup"] = await self._watch_server(cut)


def summary(rec: dict) -> str:
    cut = rec.get("cut") or {}
    recov = rec.get("recovery") or {}
    succ = rec.get("successor") or {}
    return (
        f"nested={rec.get('nested')} double={rec.get('double')} "
        f"delay={rec.get('delay')} mode={rec.get('mode')} "
        f"cut={cut.get('realized')} phase={cut.get('phase')} "
        f"begin={bool(cut.get('begin_observed'))} "
        f"c2r={cut.get('cancel_to_return')} "
        f"second={((cut.get('second') or {}).get('time') is not None)} "
        f"rec_ok={recov.get('ok')} rec_s={recov.get('seconds')} "
        f"successor={succ.get('successor_visible')} "
        f"c2s={succ.get('cancel_to_successor')} "
        f"old_cleared={((rec.get('old_cleanup') or {}).get('cleared'))} "
        f"old_s={((rec.get('old_cleanup') or {}).get('seconds'))} "
        f"total={rec.get('total_seconds')} "
        f"error={rec.get('error')}"
    )


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--modes", default="rollback,close,reconnect,fix,none")
    ap.add_argument("--delay", type=float, default=5.0)
    ap.add_argument("--second-delay", type=float, default=0.1)
    ap.add_argument("--old-cleanup-wait", type=float, default=7.0)
    ap.add_argument("--oneway", action="store_true",
                    help="delay only server->client response chunks")
    ap.add_argument("--nested", action="store_true")
    ap.add_argument("--double", action="store_true")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    args.driver = "psycopg"
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for mode in args.modes.split(","):
                sc = DelayRecovery(args, mode.strip(), args.nested, args.double, rep)
                try:
                    rec = await asyncio.wait_for(sc.run(), args.timeout * 8 + 15)
                except asyncio.CancelledError:
                    raise
                except BaseException as exc:
                    failures += 1
                    rec = {"mode": mode, "rep": rep, "error": err_text(exc),
                           "trace": __import__("traceback").format_exc()[-1600:]}
                if rec.get("error"):
                    failures += 1
                print("DELAY " + json.dumps(rec, default=str, sort_keys=True),
                      file=out, flush=True)
                print(f"DELAY-SUMMARY {summary(rec)}", flush=True)
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
