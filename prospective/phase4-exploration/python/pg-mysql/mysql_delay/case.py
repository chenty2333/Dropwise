"""mysql-connector-python 26.7.0 aio: recovery delay and re-cancellation.

Round 5, MySQL half of group 2.  One scenario process runs one
(build, target, delay, strategy, mode, rep) cell.  The target is the round-4
shape: `start_transaction()` / parameterised `REPLACE` / `commit()`, cut at the
reply of `START TRANSACTION` (top-level) or of `SAVEPOINT` inside an outer
transaction (legitimate nested scope).  A loopback proxy holds every
server->client token for 50 ms or 5 s; the target is cancelled while the proxy
is holding its reply.  Then either:

* `single`   -- run the recovery strategy to completion and try a successor;
* `recancel` -- cancel the recovery itself while its reply is held (or during
  the reconnect handshake), then try a successor and an escalation.

Measured separately, all from the monotonic timestamp of the first cancel
request and (in `recancel`) of the second:

(a) cancel request -> caller receives CancelledError;
(b) cancel request -> server transaction closed, or connection evicted
    (independent observer, ~50 ms sampling resolution);
(c) cancel request -> successor obtains a usable resource: client-side write
    rowcount + readback correct AND the independent observer sees the row.

The observer is a separate direct `mysql.connector.aio` connection through no
proxy.  Nothing here toggles a global server variable (no general log, no
performance_schema consumer changes); only a unique database/table and the
tested connection ids are touched.  A synthetic unread reply is a client-side
protocol state, not a server failure.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import statistics
import sys
import time
import traceback

import mysql.connector
import mysql.connector.aio as aio

from cases.common import close_quiet, unique_table

from .delay_proxy import DelayProxy

OUTER_ROW = "outer"
SURVIVOR_ROW = "successor"
SAVEPOINT = "dw_r5_sp"
STRATEGIES = ("rollback", "reset_session", "close_reconnect")
NESTED_STRATEGY = "savepoint_rollback_commit"
MODES = ("single", "recancel")
TARGETS = ("begin", "savepoint")

SETUP_DELAY = 0.02          # proxy s2c delay during setup / warm-up
TARGET_REPLY_TIMEOUT = 60.0
RECOVERY_TIMEOUT = 240.0    # covers 5 s/reply handshakes with margin
SUCCESSOR_TIMEOUT = 120.0
CLOSE_TIMEOUT = 60.0
CONNECT_TIMEOUT = 240.0


# --------------------------------------------------------------------------
# small async helpers
# --------------------------------------------------------------------------
async def task_outcome(task, timeout):
    """Await one task, converting hang/error/cancellation into a value.

    A CancelledError that has reached *this* coroutine (i.e. the harness or a
    wrapper is being cancelled) is re-raised; a CancelledError that only means
    the awaited child task was cancelled is returned as a value.
    """
    try:
        value = await asyncio.wait_for(task, timeout)
        return {"outcome": "ok", "value": value}
    except asyncio.CancelledError:
        cur = asyncio.current_task()
        if cur is not None and cur.cancelling():
            raise
        if task.cancelled():
            return {"outcome": "cancelled"}
        raise
    except TimeoutError:
        return {"outcome": "timeout", "error": f"{timeout:.1f}s"}
    except BaseException as exc:
        return {"outcome": "error", "error": f"{type(exc).__name__}: {exc}"}


def jsonable(value):
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    if isinstance(value, bytes):
        return value.decode("utf-8", "backslashreplace")
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [jsonable(v) for v in value]
    return repr(value)


def mono():
    return time.monotonic()


def ms_since(start, end):
    if start is None or end is None:
        return None
    return round((end - start) * 1000.0, 3)


# --------------------------------------------------------------------------
# scenario
# --------------------------------------------------------------------------
class Scenario:
    def __init__(self, args, rep: int) -> None:
        self.args = args
        self.rep = rep
        self.build = args.build
        self.target = args.target
        self.delay = args.delay
        self.strategy = args.strategy
        self.mode = args.mode
        self.table = unique_table("dw_r5_md")
        self.proxy: DelayProxy | None = None
        self.conn = None
        self.observer = None
        self.obs_cur = None
        self.cids: list[int] = []
        self.samples: list[dict] = []
        self._obs_lock = asyncio.Lock()
        self.timeline: dict = {}
        self.diag: dict = {}
        self.phases: dict = {}
        self.sampler_task: asyncio.Task | None = None
        self._stop_sampler = asyncio.Event()
        self._phase_sent = asyncio.Event()
        self._phase_marker = 0
        self.errors: list[str] = []
        self.cleanup: dict = {}
        self.t0 = mono()

    # ---- setup -------------------------------------------------------
    async def _connect(self, cfg: dict, timeout: float = CONNECT_TIMEOUT):
        return await aio.connect(**cfg, autocommit=True,
                                 connection_timeout=timeout)

    async def _exec_conn(self, conn, sql: str, params=()):
        cur = await conn.cursor()
        try:
            await cur.execute(sql, params)
            rc = cur.rowcount
            if cur.with_rows:
                await cur.fetchall()
            return rc
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _fetch_conn(self, conn, sql: str, params=()):
        cur = await conn.cursor()
        try:
            await cur.execute(sql, params)
            return [list(r) for r in await cur.fetchall()]
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _exec_obs(self, sql: str, params=()):
        async with self._obs_lock:
            await self.obs_cur.execute(sql, params)
            if self.obs_cur.with_rows:
                await self.obs_cur.fetchall()
            return self.obs_cur.rowcount

    async def _fetch_obs(self, sql: str, params=()):
        async with self._obs_lock:
            return await self._fetch_obs_unlocked(sql, params)

    async def _fetch_obs_unlocked(self, sql: str, params=()):
        await self.obs_cur.execute(sql, params)
        return [list(r) for r in await self.obs_cur.fetchall()]

    def _state(self) -> dict:
        conn = self.conn
        if conn is None:
            return {"cid": None, "in_tx": None, "unread": None,
                    "socket_connected": None}
        try:
            in_tx = bool(conn.in_transaction)
        except BaseException:
            in_tx = None
        try:
            unread = bool(conn.unread_result)
        except BaseException:
            unread = None
        try:
            sock = bool(conn.is_socket_connected())
        except BaseException:
            sock = None
        return {
            "cid": getattr(conn, "connection_id", None),
            "in_tx": in_tx,
            "unread": unread,
            "socket_connected": sock,
        }

    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=SETUP_DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), 10)
        common = dict(host=a.host, port=a.port, user=a.user,
                      password=a.password, database=a.dbname)
        self.proxy_cfg = dict(common, port=client_port)

        obs = await self._connect(common, timeout=30)
        self.observer = obs
        self.obs_cur = await obs.cursor()
        await self._exec_obs(
            f"CREATE TABLE {self.table} "
            "(k VARCHAR(32) PRIMARY KEY, v INT NOT NULL)"
        )

        conn = await self._connect(self.proxy_cfg, timeout=30)
        self.conn = conn
        if conn.connection_id:
            self.cids.append(int(conn.connection_id))

        # Warm up: plain query, plus one parameterised query so the cursor's
        # lazy internal SELECT @@session.sql_mode is cached before the target.
        await self._fetch_conn(conn, "SELECT 1")
        await self._fetch_conn(conn, "SELECT %s", (1,))
        ac = await conn.get_autocommit()
        if ac is not True:
            raise RuntimeError(f"setup autocommit is {ac!r}")

        self.timeline["t_setup_done"] = mono()
        self.proxy.set_delay(self.delay)
        self.timeline["t_target_delay_set"] = mono()
        baseline = await self._sample()
        self.samples.append(baseline)
        self.sampler_task = asyncio.create_task(self._sampler())
        self.diag["setup"] = {
            "state": self._state(),
            "proxy_port": client_port,
            "delay_after_setup": self.delay,
            "baseline": baseline,
            "server_version": (await self._fetch_obs_unlocked("SELECT VERSION()"))[0][0],
        }

    # ---- sampler -----------------------------------------------------
    async def _sample(self) -> dict:
        async with self._obs_lock:
            return await self._sample_locked()

    async def _sample_locked(self) -> dict:
        t = mono()
        out: dict = {"t": round(t, 6)}
        cids = sorted({int(c) for c in self.cids if c is not None})
        try:
            local = self._state()
        except BaseException as exc:
            local = {"error": f"{type(exc).__name__}: {exc}"}
        out["local"] = local
        if cids:
            in_list = ",".join(str(c) for c in cids)
            try:
                tx = await self._fetch_obs_unlocked(
                    "SELECT t.PROCESSLIST_ID, e.STATE, e.ACCESS_MODE, "
                    "e.AUTOCOMMIT "
                    "FROM performance_schema.events_transactions_current e "
                    "JOIN performance_schema.threads t "
                    "ON t.THREAD_ID = e.THREAD_ID "
                    f"WHERE t.PROCESSLIST_ID IN ({in_list})"
                )
                out["tx"] = {str(r[0]): r[1] for r in tx}
            except BaseException as exc:
                out["tx_error"] = f"{type(exc).__name__}: {exc}"
            try:
                trx = await self._fetch_obs_unlocked(
                    "SELECT trx_id, trx_state, trx_mysql_thread_id "
                    "FROM information_schema.innodb_trx "
                    f"WHERE trx_mysql_thread_id IN ({in_list})"
                )
                out["trx"] = len(trx)
            except BaseException as exc:
                out["trx_error"] = f"{type(exc).__name__}: {exc}"
            try:
                procs = await self._fetch_obs_unlocked(
                    "SELECT ID, COMMAND, TIME, STATE FROM "
                    "information_schema.processlist "
                    f"WHERE ID IN ({in_list})"
                )
                out["procs"] = {str(r[0]): r[1] for r in procs}
            except BaseException as exc:
                out["procs_error"] = f"{type(exc).__name__}: {exc}"
            if self.target == "savepoint":
                try:
                    locks = await self._fetch_obs_unlocked(
                        "SELECT t.PROCESSLIST_ID, COUNT(*) "
                        "FROM performance_schema.data_locks l "
                        "JOIN performance_schema.threads t "
                        "ON t.THREAD_ID = l.THREAD_ID "
                        f"WHERE t.PROCESSLIST_ID IN ({in_list}) "
                        "GROUP BY t.PROCESSLIST_ID"
                    )
                    out["locks"] = {str(r[0]): int(r[1]) for r in locks}
                except BaseException as exc:
                    out["locks_error"] = f"{type(exc).__name__}: {exc}"
        try:
            out["rows"] = await self._fetch_obs_unlocked(
                f"SELECT k, v FROM {self.table} ORDER BY k"
            )
        except BaseException as exc:
            out["rows_error"] = f"{type(exc).__name__}: {exc}"
        if self.proxy is not None:
            snap = self.proxy.snapshot()
            out["proxy"] = {
                "open": snap["open_conns"],
                "closed": snap["closed_conns"],
                "s2c": snap["s2c_read_seq"],
                "held": len(snap["held_s2c"]),
            }
        out["t_end"] = round(mono(), 6)
        return out

    async def _sampler(self) -> None:
        interval = 0.02 if self.delay < 1 else 0.1
        while not self._stop_sampler.is_set():
            try:
                self.samples.append(await self._sample())
            except BaseException as exc:
                self.samples.append({
                    "t": round(mono(), 6),
                    "sampler_error": f"{type(exc).__name__}: {exc}",
                })
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(self._stop_sampler.wait(), interval)

    async def _stop_sampler_now(self) -> None:
        self._stop_sampler.set()
        if self.sampler_task is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(self.sampler_task, 5)
            self.sampler_task = None

    # ---- target ------------------------------------------------------
    async def _target(self):
        conn = self.conn
        stages = self.diag.setdefault("target_stages", [])
        try:
            if self.target == "begin":
                self._phase_marker = self.proxy.s2c_marker()
                self._phase_sent.set()
                await conn.start_transaction()
                stages.append("start_transaction")
            else:
                await conn.start_transaction()
                stages.append("start_transaction")
                await self._exec_conn(
                    conn,
                    f"REPLACE INTO {self.table} (k, v) VALUES (%s, %s)",
                    (OUTER_ROW, 1),
                )
                stages.append("outer_write")
                self._phase_marker = self.proxy.s2c_marker()
                self._phase_sent.set()
                await self._exec_conn(conn, f"SAVEPOINT {SAVEPOINT}")
                stages.append("savepoint")
        except asyncio.CancelledError:
            self.diag["target_saw_cancel_t"] = mono()
            raise

    async def _run_target(self) -> dict:
        task = asyncio.create_task(self._target())
        phase_task = asyncio.create_task(self._phase_sent.wait())
        done, pending = await asyncio.wait(
            {phase_task, task}, timeout=TARGET_REPLY_TIMEOUT,
            return_when=asyncio.FIRST_COMPLETED,
        )
        if phase_task not in done:
            phase_task.cancel()
            with contextlib.suppress(BaseException):
                await phase_task
            if task.done():
                return {"outcome": "unrealized_target_setup",
                        "error": self._task_error(task)}
            task.cancel()
            with contextlib.suppress(BaseException):
                await task
            return {"outcome": "unrealized_phase_timeout"}
        if task.done():
            # The target completed before a cancel could be placed (e.g. the
            # reply was not held long enough).  Preserve as unrealized.
            phase_task.cancel()
            with contextlib.suppress(BaseException):
                await phase_task
            return {"outcome": "unrealized_target_completed",
                    "result": jsonable(await self._task_value(task))}

        marker = self._phase_marker
        read_task = asyncio.create_task(self.proxy.wait_s2c_read(marker, TARGET_REPLY_TIMEOUT))
        done, pending = await asyncio.wait(
            {read_task, task}, timeout=TARGET_REPLY_TIMEOUT,
            return_when=asyncio.FIRST_COMPLETED,
        )
        if read_task not in done:
            read_task.cancel()
            with contextlib.suppress(BaseException):
                await read_task
            if task.done():
                return {"outcome": "unrealized_reply_not_held",
                        "error": self._task_error(task),
                        "result": jsonable(await self._task_value(task))}
            task.cancel()
            with contextlib.suppress(BaseException):
                await task
            return {"outcome": "unrealized_reply_timeout"}

        chunk = self._last_s2c_chunk()
        t_read = chunk.read_t
        t_cancel = mono()
        self.timeline["t_target_reply_read_by_proxy"] = t_read
        self.timeline["t_target_reply_chunk"] = chunk.seq
        self.timeline["t_cancel_request"] = t_cancel
        task.cancel()
        res = await task_outcome(task, 60)
        t_caller = mono()
        t_saw = self.diag.get("target_saw_cancel_t")
        self.timeline["t_target_cancelled_seen"] = t_saw
        self.timeline["t_caller_cancelled"] = t_caller

        delivered_before = (chunk.delivered is not None and chunk.delivered
                            and chunk.deliver_t is not None
                            and chunk.deliver_t <= t_cancel)
        held_at_cancel = not delivered_before
        return {
            "outcome": res["outcome"],
            "result": jsonable(res.get("value")),
            "error": res.get("error"),
            "stages": self.diag.get("target_stages", []),
            "target_cancel_seen": t_saw is not None,
            "caller_cancelled": res["outcome"] == "cancelled",
            "forwarded_to_proxy_t": round(chunk.read_t, 6),
            "stale_reply": {
                "chunk_seq": chunk.seq,
                "chunk": chunk.as_dict(),
                "held_at_cancel": held_at_cancel,
                "delivered_before_cancel": bool(delivered_before),
                "delivery_ms_after_cancel": ms_since(t_cancel, chunk.deliver_t)
                if chunk.deliver_t is not None else None,
            },
            "a_ms": ms_since(t_cancel, t_caller),
            "target_saw_ms": ms_since(t_cancel, t_saw),
            "post_cancel_state": self._state(),
        }

    def _last_s2c_chunk(self):
        for chunk in reversed(self.proxy.chunks):
            if chunk.direction == "s2c":
                return chunk
        return None

    async def _task_value(self, task):
        if task.cancelled():
            return {"cancelled": True}
        exc = task.exception()
        if exc is not None:
            return {"error": f"{type(exc).__name__}: {exc}"}
        return task.result()

    def _task_error(self, task):
        if task.cancelled():
            return "cancelled"
        exc = task.exception()
        if exc is not None:
            return f"{type(exc).__name__}: {exc}"
        return None

    # ---- recovery ----------------------------------------------------
    def _record_chunks(self, marker: dict) -> list[dict]:
        return self.proxy.chunk_window(marker["chunks"])

    async def _recovery_once(self) -> dict:
        t0 = mono()
        marker = self.proxy.mark()
        out: dict = {"strategy": self.strategy, "t_start": round(t0, 6),
                     "marker": marker}
        if self.conn is None:
            out.update({"outcome": "error", "error": "no handle"})
            return out
        if self.strategy == "rollback":
            res = await task_outcome(
                asyncio.create_task(self.conn.rollback()), RECOVERY_TIMEOUT
            )
            out.update(res)
            out["return_t"] = round(mono(), 6)
        elif self.strategy == "reset_session":
            res = await task_outcome(
                asyncio.create_task(self.conn.reset_session()), RECOVERY_TIMEOUT
            )
            out.update(res)
            out["return_t"] = round(mono(), 6)
        elif self.strategy == "close_reconnect":
            old_cid = getattr(self.conn, "connection_id", None)
            close_res = await task_outcome(
                asyncio.create_task(self.conn.close()), CLOSE_TIMEOUT
            )
            out["close"] = close_res
            out["close_ms"] = ms_since(t0, mono())
            out["old_cid"] = old_cid
            self.diag["recovery_close_ms"] = out["close_ms"]
            self.diag["recovery_close_t"] = mono()
            self.conn = None
            self.diag["recovery_connect_start_t"] = mono()
            connect_res = await task_outcome(
                asyncio.create_task(self._connect(self.proxy_cfg)),
                CONNECT_TIMEOUT,
            )
            out["connect"] = jsonable(connect_res) if connect_res["outcome"] != "ok" else {"outcome": "ok"}
            if connect_res["outcome"] == "ok":
                self.conn = connect_res["value"]
                if self.conn.connection_id:
                    self.cids.append(int(self.conn.connection_id))
                out["new_cid"] = int(self.conn.connection_id)
            out["outcome"] = "ok" if connect_res["outcome"] == "ok" else connect_res["outcome"]
            if connect_res["outcome"] != "ok":
                out["error"] = connect_res.get("error")
            out["connect_ms"] = ms_since(t0, mono())
            out["return_t"] = round(mono(), 6)
        elif self.strategy == NESTED_STRATEGY:
            if self.target != "savepoint":
                raise RuntimeError("savepoint strategy requires --target savepoint")
            sp = await task_outcome(
                asyncio.create_task(
                    self._exec_conn(self.conn, f"ROLLBACK TO SAVEPOINT {SAVEPOINT}")
                ),
                RECOVERY_TIMEOUT,
            )
            out["savepoint_rollback"] = sp
            commit = await task_outcome(
                asyncio.create_task(self.conn.commit()), RECOVERY_TIMEOUT
            )
            out["commit"] = commit
            out["outcome"] = ("ok" if sp["outcome"] == "ok"
                              and commit["outcome"] == "ok"
                              else "error")
            out["return_t"] = round(mono(), 6)
        else:
            raise RuntimeError(f"unknown strategy {self.strategy}")
        out["recovery_ms"] = ms_since(t0, mono())
        out["post_state"] = self._state()
        out["chunks"] = self._record_chunks(marker)
        out["t_end"] = round(mono(), 6)
        return out

    async def _recovery_wrapper(self):
        self.diag["recovery_entered_t"] = mono()
        try:
            return await self._recovery_once()
        except asyncio.CancelledError:
            self.diag["recovery_saw_cancel_t"] = mono()
            raise

    # ---- successor ---------------------------------------------------
    async def _successor_once(self, label: str) -> dict:
        t0 = mono()
        out: dict = {"label": label, "t_start": round(t0, 6)}
        if self.conn is None:
            out.update({"usable": False, "reason": "no handle"})
            out["end_t"] = round(mono(), 6)
            return out
        write = await task_outcome(
            asyncio.create_task(
                self._exec_conn(
                    self.conn,
                    f"REPLACE INTO {self.table} (k, v) VALUES (%s, %s)",
                    (SURVIVOR_ROW, 7),
                )
            ),
            SUCCESSOR_TIMEOUT,
        )
        out["write"] = write
        out["write_t"] = round(mono(), 6)
        out["write_ms"] = ms_since(t0, mono())
        read = None
        if write["outcome"] == "ok":
            read = await task_outcome(
                asyncio.create_task(
                    self._fetch_conn(
                        self.conn,
                        f"SELECT v FROM {self.table} WHERE k = %s",
                        (SURVIVOR_ROW,),
                    )
                ),
                SUCCESSOR_TIMEOUT,
            )
            out["read"] = read
        out["read_t"] = round(mono(), 6)
        out["read_ms"] = ms_since(t0, mono()) if read is not None else None
        out["state_after"] = self._state()
        out["end_t"] = round(mono(), 6)
        out["client_ok"] = (
            write["outcome"] == "ok"
            and write.get("value") in (1, 2)
            and read is not None
            and read["outcome"] == "ok"
            and jsonable(read.get("value")) == [[7]]
        )
        # Observer visibility is filled in from the sample series after the
        # sampler is stopped (no concurrent use of the observer cursor).
        return out

    async def _escalate(self) -> dict:
        out: dict = {"t_start": round(mono(), 6)}
        t0 = mono()
        if self.conn is not None:
            close_res = await task_outcome(
                asyncio.create_task(self.conn.close()), CLOSE_TIMEOUT
            )
            out["close"] = close_res
            out["close_ms"] = ms_since(t0, mono())
            self.conn = None
        connect_res = await task_outcome(
            asyncio.create_task(self._connect(self.proxy_cfg)), CONNECT_TIMEOUT
        )
        out["connect_outcome"] = connect_res["outcome"]
        if connect_res["outcome"] == "ok":
            self.conn = connect_res["value"]
            if self.conn.connection_id:
                self.cids.append(int(self.conn.connection_id))
            out["new_cid"] = int(self.conn.connection_id)
        else:
            out["connect_error"] = connect_res.get("error")
        out["connect_ms"] = ms_since(t0, mono())
        out["successor"] = await self._successor_once("escalation")
        out["end_t"] = round(mono(), 6)
        out["usable"] = bool(out["successor"].get("client_ok"))
        return out

    # ---- recancel ----------------------------------------------------
    async def _recovery_suspension(self, task, marker):
        """Wait until the recovery is suspended (reply held or stream blocked).

        Returns (kind, evidence).  kind is one of:
          reply_held   -- a new server->client chunk was read by the proxy and
                          is being held; the recovery is awaiting a reply
          stream_blocked -- the recovery has sent a command and is suspended,
                          but an earlier reply is still held ahead of it
          task_done    -- recovery returned before a cancel could be placed
          timeout      -- no suspension observed
        """
        read_task = asyncio.create_task(
            self.proxy.wait_s2c_read(marker["s2c_read_seq"], 120.0)
        )
        # Short 50 ms cells: cancel well before the recovery reply is due, so
        # the second cancel lands while the stream is still blocked (the
        # target reply is held).  Long 5 s cells: 50 ms after recovery start.
        timer_s = 0.01 if self.delay < 0.2 else 0.05
        timer = asyncio.create_task(asyncio.sleep(timer_s))
        try:
            done, _ = await asyncio.wait(
                {read_task, task, timer}, timeout=120.0,
                return_when=asyncio.FIRST_COMPLETED,
            )
            if task in done:
                return "task_done", {}
            if read_task in done:
                chunk = self._last_s2c_chunk()
                return "reply_held", {"chunk": chunk.as_dict() if chunk else None}
            # The early timer elapsed.  If the recovery is still suspended and
            # has sent a command on this connection, a held earlier reply is
            # enough evidence of a real suspension for same-connection
            # recoveries.
            if self.strategy in ("rollback", "reset_session"):
                held = [c for c in self.proxy.chunks
                        if c.direction == "s2c" and not c.delivered
                        and c.error is None]
                sent = [c for c in self.proxy.chunks[marker["chunks"]:]
                        if c.direction == "c2s"]
                if held and sent:
                    return "stream_blocked", {
                        "held_chunk": held[-1].as_dict(),
                        "commands_sent": [c.as_dict() for c in sent],
                    }
            # Otherwise wait for the recovery's own reply (or task end).
            done, _ = await asyncio.wait(
                {read_task, task}, timeout=120.0,
                return_when=asyncio.FIRST_COMPLETED,
            )
            if task in done:
                return "task_done", {}
            if read_task in done:
                chunk = self._last_s2c_chunk()
                return "reply_held", {"chunk": chunk.as_dict() if chunk else None}
            return "timeout", {}
        finally:
            for t in (read_task, timer):
                if not t.done():
                    t.cancel()
            for t in (read_task, timer):
                with contextlib.suppress(BaseException):
                    await t

    async def _recancel_recovery(self) -> dict:
        task = asyncio.create_task(self._recovery_wrapper())
        marker = self.proxy.mark()
        out: dict = {"recovery_marker": marker}
        kind, evidence = await self._recovery_suspension(task, marker)
        if kind != "reply_held" and kind != "stream_blocked":
            res = await task_outcome(task, 5)
            out["realized"] = False
            out["reason"] = {
                "task_done": "recovery completed before a cancelable suspension",
                "timeout": "no suspension observed within 120 s",
            }.get(kind, kind)
            out["recovery_outcome"] = res
            out["recovery_saw_cancel"] = False
            out["t_cancel_request"] = None
            out["recovery_commands_forwarded"] = [
                c.as_dict() for c in self.proxy.chunks[marker["chunks"]:]
                if c.direction == "c2s"
            ]
            out["post_state"] = self._state()
            return out

        t_cancel2 = mono()
        self.timeline["t_cancel2_request"] = t_cancel2
        out.update({
            "realized": True,
            "suspension_kind": kind,
            "suspension_evidence": evidence,
            "t_cancel_request": round(t_cancel2, 6),
            "recovery_task_done_before_cancel": task.done(),
            "recovery_commands_forwarded": [
                c.as_dict() for c in self.proxy.chunks[marker["chunks"]:]
                if c.direction == "c2s"
            ],
        })
        task.cancel()
        res = await task_outcome(task, 120)
        t_caller2 = mono()
        saw = self.diag.get("recovery_saw_cancel_t")
        self.timeline["t_cancel2_caller_cancelled"] = t_caller2
        out.update({
            "recovery_outcome": res,
            "recovery_saw_cancel": saw is not None,
            "t_recovery_saw_cancel": round(saw, 6) if saw else None,
            "t_caller_cancelled": round(t_caller2, 6),
            "a2_ms": ms_since(t_cancel2, t_caller2),
            "recovery_cancel_seen_ms": ms_since(t_cancel2, saw),
            "post_state": self._state(),
            "chunks": self._record_chunks(marker),
        })
        return out

    # ---- driver ------------------------------------------------------
    async def run(self) -> dict:
        rec: dict = {
            "meta": {
                "round": "r5-mysql-delay",
                "build": self.build,
                "target": self.target,
                "delay_s": self.delay,
                "strategy": self.strategy,
                "mode": self.mode,
                "rep": self.rep,
                "table": self.table,
                "argv": sys.argv,
                "connector_version": mysql.connector.__version__,
                "connector_file": mysql.connector.__file__,
                "python": sys.version.split()[0],
                "pid": os.getpid(),
                "t_wall_start": round(time.time(), 6),
                "t_mono_start": round(self.t0, 6),
            },
            "setup": None,
            "target": None,
            "post_cancel": None,
            "recovery": None,
            "second_cancel": None,
            "successor": None,
            "escalation": None,
            "resource": None,
            "proxy": None,
            "samples": self.samples,
            "cleanup": None,
            "errors": self.errors,
        }
        try:
            await asyncio.wait_for(self.setup(), 60)
            rec["setup"] = self.diag.get("setup")
            rec["target"] = await self._run_target()
            post_sample = await self._sample()
            self.samples.append(post_sample)
            rec["post_cancel"] = {
                "state": self._state(),
                "sample": post_sample,
                "proxy": self.proxy.snapshot(include_chunks=False),
            }

            if self.mode == "single":
                t_rec_start = mono()
                res = await task_outcome(
                    asyncio.create_task(self._recovery_wrapper()),
                    RECOVERY_TIMEOUT,
                )
                self.timeline["t_recovery_start"] = t_rec_start
                self.timeline["t_recovery_return"] = mono()
                if res["outcome"] == "ok":
                    rec["recovery"] = res["value"]
                else:
                    rec["recovery"] = {
                        "outcome": res["outcome"],
                        "error": res.get("error"),
                        "recovery_ms": ms_since(t_rec_start, mono()),
                        "post_state": self._state(),
                    }
                rec["recovery"]["task_outcome"] = res["outcome"]
                rec["recovery"]["saw_cancel"] = (
                    self.diag.get("recovery_saw_cancel_t") is not None
                )
                rec["recovery"]["from_cancel_request_ms"] = ms_since(
                    self.timeline.get("t_cancel_request"),
                    self.timeline.get("t_recovery_return"),
                )
                rec["successor"] = await self._successor_once("primary")
                if (not rec["successor"].get("client_ok")
                        and (self.args.escalate or self.delay < 1)):
                    rec["escalation"] = await self._escalate()
            else:
                rec["second_cancel"] = await self._recancel_recovery()
                rec["successor"] = await self._successor_once("after_recancel")
                if (not rec["successor"].get("client_ok")
                        and (self.args.escalate or self.delay < 1)):
                    rec["escalation"] = await self._escalate()

            # Let the sampler see the final server state, then stop it.
            await asyncio.sleep(0.15)
        except BaseException as exc:
            self.errors.append(f"{type(exc).__name__}: {exc}")
            rec["traceback"] = traceback.format_exc()
        finally:
            await self._stop_sampler_now()
            rec["samples"] = self.samples
            try:
                rec["resource"] = self._derive_resource(rec)
            except BaseException as exc:
                rec["resource"] = {"error": f"{type(exc).__name__}: {exc}"}
            rec["proxy"] = (self.proxy.snapshot(include_chunks=True)
                            if self.proxy else None)
            rec["meta"]["cids"] = list(self.cids)
            await self._cleanup()
            rec["cleanup"] = self.cleanup
            if isinstance(rec.get("resource"), dict):
                tc = self.timeline.get("t_cancel_request")
                if tc is not None and self.cleanup.get("close_start_t"):
                    rec["resource"]["cleanup_close_ms"] = ms_since(
                        tc, self.cleanup["close_start_t"])
                    rec["resource"]["cleanup_close_end_ms"] = ms_since(
                        tc, self.cleanup["close_end_t"])
            rec["timeline"] = jsonable(self.timeline)
            rec["diag"] = jsonable(self.diag)
            rec["seconds"] = round(mono() - self.t0, 3)
        return rec

    # ---- resource derivation ----------------------------------------
    def _derive_resource(self, rec: dict) -> dict:
        tc = self.timeline.get("t_cancel_request")
        if tc is None:
            return {"unknown": "no cancel timestamp"}
        if not self.cids:
            return {"unknown": "no connection id"}
        target_cid = int(self.cids[0])
        esc_start = (rec.get("escalation") or {}).get("t_start")
        # A close performed by the escalation fallback is not the tested
        # recovery; exclude it from "b".  With no escalation, the whole
        # observed span is attributable to the tested recovery.
        cutoff = esc_start if esc_start is not None else float("inf")

        all_samples = sorted((s for s in self.samples if "t" in s),
                             key=lambda s: s["t"])
        before = [s for s in all_samples if s["t"] < tc]
        after = [s for s in all_samples if s["t"] >= tc]
        was_present = any(str(target_cid) in (s.get("procs") or {}) for s in before)

        tx_series = [(s["t"], (s.get("tx") or {}).get(str(target_cid)))
                     for s in after if "tx" in s]
        proc_series = [(s["t"], str(target_cid) in (s.get("procs") or {}))
                       for s in after if "procs" in s]

        active_t = None
        tx_closed_t = None
        for t, st in tx_series:
            if t > cutoff:
                break
            if active_t is None and st == "ACTIVE":
                active_t = t
            elif active_t is not None and st != "ACTIVE":
                tx_closed_t = t
                break

        proc_evicted_t = None
        seen_present = was_present
        for t, present in proc_series:
            if t > cutoff:
                break
            if present:
                seen_present = True
            elif seen_present:
                proc_evicted_t = t
                break

        sock_false_t = None
        for s in after:
            if s["t"] > cutoff:
                break
            local = s.get("local") or {}
            if (local.get("cid") == target_cid
                    and local.get("socket_connected") is False):
                sock_false_t = s["t"]
                break

        proxy_close = []
        if self.proxy is not None:
            for t in self.proxy.conn_close_t.values():
                if tc <= t <= cutoff:
                    proxy_close.append(t)
            for t in self.proxy.upstream_close_t.values():
                if tc <= t <= cutoff:
                    proxy_close.append(t)
        proxy_evicted_t = min(proxy_close) if proxy_close else None

        eviction_candidates = [t for t in (proc_evicted_t, sock_false_t,
                                           proxy_evicted_t) if t is not None]
        evicted_t = min(eviction_candidates) if eviction_candidates else None

        b_t = None
        b_kind = "unknown"
        if tx_closed_t is not None and evicted_t is not None:
            b_t = min(tx_closed_t, evicted_t)
            b_kind = ("both" if abs(tx_closed_t - evicted_t) < 0.002
                      else ("tx_closed" if tx_closed_t < evicted_t
                            else "evicted"))
        elif tx_closed_t is not None:
            b_t, b_kind = tx_closed_t, "tx_closed"
        elif evicted_t is not None:
            b_t, b_kind = evicted_t, "evicted"

        rows_first = {}
        for s in after:
            for row in s.get("rows") or []:
                if len(row) >= 2 and row[0] not in rows_first:
                    rows_first[row[0]] = s["t"]

        result = {
            "b_kind": b_kind,
            "b_ms": ms_since(tc, b_t),
            "tx_closed_ms": ms_since(tc, tx_closed_t),
            "tx_active_first_ms": ms_since(tc, active_t),
            "proc_evicted_ms": ms_since(tc, proc_evicted_t),
            "sock_false_ms": ms_since(tc, sock_false_t),
            "proxy_evicted_ms": ms_since(tc, proxy_evicted_t),
            "target_cid": target_cid,
            "was_present_before_cancel": was_present,
            "post_cancel_socket": (rec.get("post_cancel") or {})
            .get("state", {}).get("socket_connected"),
            "span_ms": ms_since(tc, mono()),
            "rows_first_seen_ms": {
                str(k): ms_since(tc, v) for k, v in rows_first.items()
            },
            "outer_visible_ms": ms_since(tc, rows_first.get(OUTER_ROW)),
            "successor_visible_ms": ms_since(tc, rows_first.get(SURVIVOR_ROW)),
        }

        # Usability: client-side correctness plus independent visibility.
        primary = rec.get("successor")
        esc = rec.get("escalation") or {}
        esc_succ = esc.get("successor")
        esc_start = esc.get("t_start")
        vis_t = rows_first.get(SURVIVOR_ROW)
        primary_start = (primary or {}).get("t_start")
        primary_end = esc_start if esc_start is not None else None
        if primary is not None:
            primary["observer_visible"] = bool(
                vis_t is not None and primary_start is not None
                and vis_t >= primary_start
                and (primary_end is None or vis_t < primary_end)
            )
            primary["observer_visible_after_phase"] = bool(
                vis_t is not None and primary_end is not None
                and vis_t >= primary_end
            )
            primary["usable"] = bool(primary.get("client_ok")
                                     and vis_t is not None)
            primary["usable_ms"] = (
                ms_since(tc, max(primary.get("read_t") or 0.0,
                                 vis_t or 0.0))
                if primary["usable"] else None
            )
            primary["client_write_rowcount"] = (
                (primary.get("write") or {}).get("value")
                if (primary.get("write") or {}).get("outcome") == "ok" else None
            )
            primary["client_readback"] = (
                (primary.get("read") or {}).get("value")
                if (primary.get("read") or {}).get("outcome") == "ok" else None
            )
        if esc_succ is not None:
            esc_succ["observer_visible"] = bool(
                vis_t is not None and esc_start is not None
                and vis_t >= esc_start
            )
            esc_succ["observer_visible_before_phase"] = bool(
                vis_t is not None and esc_start is not None
                and vis_t < esc_start
            )
            esc_succ["usable"] = bool(esc_succ.get("client_ok")
                                      and vis_t is not None)
            esc_succ["usable_ms"] = (
                ms_since(tc, max(esc_succ.get("read_t") or 0.0,
                                 vis_t or 0.0))
                if esc_succ["usable"] else None
            )
            esc_succ["client_write_rowcount"] = (
                (esc_succ.get("write") or {}).get("value")
                if (esc_succ.get("write") or {}).get("outcome") == "ok" else None
            )
            esc_succ["client_readback"] = (
                (esc_succ.get("read") or {}).get("value")
                if (esc_succ.get("read") or {}).get("outcome") == "ok" else None
            )
        return result

    # ---- cleanup -----------------------------------------------------
    async def _cleanup(self) -> None:
        out: dict = {}
        if self.conn is not None:
            out["close_start_t"] = mono()
            out["close"] = await task_outcome(
                asyncio.create_task(self.conn.close()), CLOSE_TIMEOUT
            )
            out["close_end_t"] = mono()
            self.conn = None
        if self.observer is not None and self.obs_cur is not None:
            try:
                await self._exec_obs(f"DROP TABLE IF EXISTS {self.table}")
                out["drop_table"] = "ok"
            except BaseException as exc:
                out["drop_table"] = f"{type(exc).__name__}: {exc}"
            out["observer_close"] = await close_quiet(self.observer.close())
            self.observer = None
        if self.proxy is not None:
            out["proxy_close"] = await close_quiet(self.proxy.close())
        self.cleanup = out


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------
def _assert_build(build: str) -> dict:
    f = mysql.connector.__file__ or ""
    is_patched = "/fix/patched/" in f
    if build == "patched" and not is_patched:
        raise SystemExit(
            f"--build patched but mysql.connector is {f!r}; "
            "put fix/patched first on PYTHONPATH"
        )
    if build == "original" and is_patched:
        raise SystemExit(
            f"--build original but mysql.connector is {f!r}; "
            "remove fix/patched from PYTHONPATH"
        )
    import mysql.connector.aio.network as network
    import mysql.connector.aio.abstracts as abstracts
    with open(network.__file__, encoding="utf-8") as fh:
        source = fh.read()
    return {
        "mysql_connector_file": f,
        "aio_network_file": network.__file__,
        "aio_abstracts_file": abstracts.__file__,
        "close_on_cancel_in_source": "_close_on_cancel" in source,
        "patched_path": is_patched,
    }


async def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--user", default="root")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", required=True)
    p.add_argument("--build", required=True, choices=("original", "patched"))
    p.add_argument("--target", required=True, choices=TARGETS)
    p.add_argument("--delay", type=float, required=True,
                   help="server->client delay in seconds (e.g. 0.05 or 5.0)")
    p.add_argument("--strategy", required=True,
                   choices=STRATEGIES + (NESTED_STRATEGY,))
    p.add_argument("--mode", required=True, choices=MODES)
    p.add_argument("--reps", type=int, default=1)
    p.add_argument("--jsonl", default=None)
    p.add_argument("--escalate", action="store_true",
                   help="close+reconnect after a failed primary successor "
                        "(automatic for short-delay cells)")
    p.add_argument("--label", default="")
    args = p.parse_args()
    if args.strategy == NESTED_STRATEGY and args.target != "savepoint":
        p.error("--strategy savepoint_rollback_commit requires --target savepoint")

    build_info = _assert_build(args.build)
    header = {
        "event": "start",
        "build_info": build_info,
        "argv": sys.argv,
        "server": f"{args.host}:{args.port}",
        "dbname": args.dbname,
        "delay": args.delay,
        "target": args.target,
        "strategy": args.strategy,
        "mode": args.mode,
        "reps": args.reps,
    }
    print("START " + json.dumps(jsonable(header)), flush=True)

    for rep in range(1, args.reps + 1):
        sc = Scenario(args, rep)
        rec = await sc.run()
        rec["meta"]["build_info"] = build_info
        line = json.dumps(jsonable(rec), sort_keys=False)
        if args.jsonl:
            with open(args.jsonl, "a", encoding="utf-8") as fh:
                fh.write(line + "\n")
        summary = {
            "build": args.build,
            "target": args.target,
            "delay": args.delay,
            "strategy": args.strategy,
            "mode": args.mode,
            "rep": rep,
            "target_outcome": (rec.get("target") or {}).get("outcome"),
            "a_ms": (rec.get("target") or {}).get("a_ms"),
            "b_ms": (rec.get("resource") or {}).get("b_ms"),
            "b_kind": (rec.get("resource") or {}).get("b_kind"),
            "primary_usable": (rec.get("successor") or {}).get("usable"),
            "primary_usable_ms": (rec.get("successor") or {}).get("usable_ms"),
            "escalation_usable": ((rec.get("escalation") or {})
                                  .get("successor") or {}).get("usable"),
            "second_realized": (rec.get("second_cancel") or {}).get("realized"),
            "second_saw_recovery": (rec.get("second_cancel") or {})
            .get("recovery_saw_cancel"),
            "errors": rec.get("errors"),
            "seconds": rec.get("seconds"),
        }
        print("RESULT " + json.dumps(jsonable(summary)), flush=True)
    print("DONE", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
