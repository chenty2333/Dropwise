"""Round-4: official transaction context managers under cancellation.

Question: when a task is cancelled inside `async with conn.transaction():`,
which phase actually stops -- context entry (BEGIN), the body statement, context
exit (COMMIT), or context exit on the error path (ROLLBACK)?  What state does
each phase hand to the next user, and is the documented rollback guarantee still
worth anything at that point?

This module drives the context manager manually (`await tx.__aenter__()`,
`await tx.__aexit__(...)`) so the phase that was running when the cancellation
landed is recorded *at cancellation time*, not guessed afterwards.  The manual
drive is semantically identical to `async with`: on an entry failure `__aexit__`
is not called; on a body failure it is called with the exception; on normal exit
it is called with None.

Two drivers, direct connection only (no pool):
  psycopg 3.3.6      AsyncConnection / AsyncTransaction
  asyncpg 0.32.0     Connection / Transaction

Modes:
  --count                 print uncancelled target yields K
  --probe-phase 1,2,...   per-k phase, client/server state, optional recovery,
                          then a retry through the same official CM
  --double-cancel         same, but cancel once more at the first suspension
                          after the initial cancellation (cleanup phase)
  --ladder RECOVERY       B1/B2/B3/M1 with recovery=none|rollback|reconnect

The independent observer is a separate direct connection that reads the table
and pg_stat_activity; it never goes through the proxy or the tested handle.
Pending: DelayProxy, 50 ms per server->client chunk, tested connection only.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import sys
import time

import asyncpg
import psycopg

from .common import (  # noqa: E402  (also puts research/python-retro on sys.path)
    BOUND,
    DELAY,
    DelayProxy,
    add_db_args,
    add_event,
    bound,
    close_quiet,
    compact,
    err_text,
    must_ok,
    sanitize,
    unique_table,
)
from dropwise_py import (  # noqa: E402
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    normalize,
    print_report,
)
from dropwise_py.engine import (  # noqa: E402
    OpResult,
    PureTask,
    TargetRun,
    _await_target,
)

TARGET_ROW = "target"
SURVIVOR_ROW = "survivor"
B3_ROW = "b3"
RETRY_ROW = "retry"

TX_NAMES = {0: "IDLE", 1: "ACTIVE", 2: "INTRANS", 3: "INERROR", 4: "UNKNOWN"}


def tx_name(value) -> str:
    try:
        return TX_NAMES.get(int(value), str(value))
    except Exception:
        return str(value)


class _PhasedTask(PureTask):
    """CutTask that records the scenario phase when each cancel is requested.

    `holder["phase"]` is set by the scenario immediately before each await that
    can be cut.  Reading it inside `cancel()` (the task is suspended at that
    moment) records where the cut actually landed.  With `double=True` a second
    cancel is issued at the next suspension after the first one, i.e. inside the
    driver's cleanup/rollback work.
    """

    def __init__(self, coro, *, cut_after, holder, double=False):
        self.steps = 0
        self.yields = 0
        self.cut_after = cut_after
        self.cut_requested = False
        self.holder = holder
        self.double = double
        self.cut_phase: str | None = None
        self.second_phase: str | None = None
        self.second_requested = False
        self.second_pending = False
        super().__init__(coro)

    def _Task__step(self, exc=None) -> None:  # name-mangled override
        self.steps += 1
        super()._Task__step(exc)
        if self.done():
            return
        self.yields += 1
        if not self.cut_requested and self.yields == self.cut_after:
            self.cut_requested = True
            self.cut_phase = self.holder.get("phase")
            self.cancel()
            if self.double:
                self.second_pending = True
            return
        if self.second_pending and not self.second_requested:
            self.second_requested = True
            self.second_phase = self.holder.get("phase")
            self.cancel()


async def _await_phased(
    coro, *, cut_after: int, timeout: float, holder: dict, double: bool
) -> tuple[TargetRun, dict]:
    started = time.monotonic()
    task = _PhasedTask(coro, cut_after=cut_after, holder=holder, double=double)
    outcome = "error"
    result = None
    error = None
    try:
        result = await asyncio.wait_for(task, timeout)
        if getattr(result, "ok", True) is False:
            outcome = "error"
            error = f"target reported failure: {result!r}"
        else:
            outcome = "completed"
    except asyncio.CancelledError:
        outcome = "cancelled"
    except asyncio.TimeoutError:
        outcome = "timeout"
        error = f"timeout after {timeout:.1f}s"
        task.second_pending = False
        task.cancel()
        with contextlib.suppress(BaseException):
            await task
    except BaseException as exc:
        outcome = "error"
        error = err_text(exc)
    finally:
        task.second_pending = False
    run = TargetRun(
        outcome=outcome,
        result=result,
        error=error,
        yields=task.yields,
        cut_requested=task.cut_requested,
        cut_delivered=task.cut_requested,
        seconds=time.monotonic() - started,
    )
    meta = {
        "cut_phase": task.cut_phase,
        "second_phase": task.second_phase,
        "second_requested": task.second_requested,
    }
    return run, meta


def install_psycopg_enter_fix() -> None:
    """Experimental driver-level repair: make a cancelled __aenter__ roll back.

    Runtime monkeypatch only; the original package on disk is untouched.  The
    patch runs only in the explicit --fix-enter variant and is never used to
    produce the original measurements.
    """
    from psycopg.transaction import AsyncTransaction, BaseTransaction

    original = AsyncTransaction.__aenter__

    async def fixed(self):
        try:
            return await original(self)
        except BaseException as exc:
            if self.status is BaseTransaction.Status.ACTIVE:
                try:
                    async with self._conn.lock:
                        await self._conn.wait(self._rollback_gen(exc))
                except BaseException:
                    # A second cancellation during the repair: fail closed.
                    with contextlib.suppress(BaseException):
                        await self._conn.close()
            raise

    AsyncTransaction.__aenter__ = fixed


def install_asyncpg_enter_fix() -> None:
    """Fail-closed experimental repair: a half-entered transaction is killed.

    A rollback attempted *inside* the cancelled __aenter__ was not reliable in
    this version: the cancellation machinery may still be in flight and the
    BEGIN reply may not have been consumed yet, so a ROLLBACK can be matched
    against stale protocol state.  Terminating the connection is the minimal
    repair that cannot leave a silently nestable transaction behind.
    """
    from asyncpg.transaction import Transaction, TransactionState

    original = Transaction.__aenter__

    async def fixed(self):
        try:
            return await original(self)
        except BaseException:
            con = self._connection
            with contextlib.suppress(BaseException):
                con.terminate()
            if getattr(con, "_top_xact", None) is self:
                con._top_xact = None
            self._state = TransactionState.FAILED
            raise

    Transaction.__aenter__ = fixed


class CmScenario(Scenario):
    """One direct handle, official transaction CM as target/retry/survivor."""

    def __init__(self, args) -> None:
        self.args = args
        self.driver = args.driver
        self.recovery = getattr(args, "recovery", "none")
        self.table = unique_table("dw_r4_cm")
        self.proxy: DelayProxy | None = None
        self.proxy_port: int | None = None
        self.conn = None
        self.observer = None
        self.tx = None
        self.holder: dict = {"phase": "setup"}
        self.diag: dict = {}
        self.events: list[dict] = []
        self.dsn_direct: str | None = None
        self.dsn_proxy: str | None = None
        self.cut_meta: dict | None = None
        self.recovery_done = False
        self.pid: int | None = None

    # ---- connections -------------------------------------------------
    def _dsn(self, port: int) -> str:
        a = self.args
        if self.driver == "psycopg":
            return (
                f"host={a.host} port={port} user={a.user} password={a.password} "
                f"dbname={a.dbname} connect_timeout=5"
            )
        return f"postgresql://{a.user}:{a.password}@{a.host}:{port}/{a.dbname}"

    async def _tested_connect(self, port: int):
        if self.driver == "psycopg":
            return await psycopg.AsyncConnection.connect(
                self._dsn(port), autocommit=True
            )
        return await asyncpg.connect(self._dsn(port), timeout=BOUND)

    async def _observer_connect(self):
        return await self._tested_connect(self.args.port)

    async def _conn_pid(self) -> int:
        if self.driver == "psycopg":
            return self.conn.info.backend_pid
        return self.conn.get_server_pid()

    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        self.proxy_port = client_port
        self.observer = must_ok("observer connect", await bound(self._observer_connect()))
        created = await bound(
            self.observer.execute(
                f"CREATE TABLE {self.table} (k text PRIMARY KEY, v integer NOT NULL)"
            )
        )
        must_ok("create table", created)
        self.conn = must_ok("tested connect", await bound(self._tested_connect(client_port)))
        self.pid = await self._conn_pid()
        self.diag["setup_pid"] = self.pid
        # Preflight: the exact normal path (CM write + independent observer) must
        # work before any cut can be called a finding.
        normal = await self._cm_write(TARGET_ROW, 1)
        if not normal.ok:
            raise RuntimeError(f"CM preflight failed: {normal!r}")
        rows = await self._observer_rows()
        if rows != [[TARGET_ROW, 1]]:
            raise RuntimeError(f"CM preflight not observed: {rows!r}")
        deleted = await bound(
            self.observer.execute(f"DELETE FROM {self.table}")
        )
        must_ok("preflight delete", deleted)
        empty = await self._observer_rows()
        if empty != []:
            raise RuntimeError(f"preflight cleanup not empty: {empty!r}")
        self.diag["preflight"] = {"write": normal.value, "rows": rows}
        self.events.clear()
        add_event(self.events, "after_setup", pid=self.pid)

    # ---- state helpers ----------------------------------------------
    def _client_state(self) -> dict:
        if self.driver == "psycopg":
            tx = self.tx
            out = {
                "driver": "psycopg",
                "status": tx_name(self.conn.info.transaction_status),
                "num_transactions": getattr(self.conn, "_num_transactions", None),
            }
            if tx is not None:
                status = getattr(tx, "status", None)
                out["tx_status"] = getattr(status, "value", None) or repr(status)
            return out
        tx = self.tx
        try:
            in_tx = self.conn.is_in_transaction()
            closed = self.conn.is_closed()
        except BaseException as exc:
            in_tx = f"unavailable: {type(exc).__name__}"
            closed = True
        return {
            "driver": "asyncpg",
            "in_transaction": in_tx,
            "closed": closed,
            "tx_state": getattr(getattr(tx, "_state", None), "name", None),
            "top_xact_is_tx": getattr(self.conn, "_top_xact", None) is tx,
        }

    async def _observer_rows(self):
        if self.driver == "psycopg":
            cur = await self.observer.execute(
                f"SELECT k, v FROM {self.table} ORDER BY k"
            )
            rows = await cur.fetchall()
            return [[r[0], r[1]] for r in rows]
        rows = await self.observer.fetch(
            f"SELECT k, v FROM {self.table} ORDER BY k"
        )
        return [[r["k"], r["v"]] for r in rows]

    async def _server_sample(self, pid: int | None = None) -> dict:
        pid = self.pid if pid is None else pid
        if pid is None:
            return {"present": False}
        try:
            if self.driver == "psycopg":
                cur = await self.observer.execute(
                    "SELECT state, backend_xid::text, "
                    "       (xact_start IS NOT NULL)::text, left(query, 120) "
                    "FROM pg_stat_activity WHERE pid = %s",
                    (pid,),
                )
                row = await cur.fetchone()
            else:
                rec = await self.observer.fetchrow(
                    "SELECT state, backend_xid::text, "
                    "       (xact_start IS NOT NULL)::text, left(query, 120) "
                    "FROM pg_stat_activity WHERE pid = $1",
                    pid,
                )
                row = None if rec is None else tuple(rec)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}
        if row is None:
            return {"pid": pid, "present": False}
        return {
            "pid": pid,
            "state": row[0],
            "backend_xid": row[1],
            "in_xact": row[2],
            "query": row[3],
        }

    # ---- the official CM, manually driven ---------------------------
    def _sql_upsert(self) -> str:
        return (
            f"INSERT INTO {self.table} (k, v) VALUES ($1, $2) "
            "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v"
        ) if self.driver == "asyncpg" else (
            f"INSERT INTO {self.table} (k, v) VALUES (%s, %s) "
            "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v"
        )

    async def _execute_upsert(self, row: str, value: int) -> str:
        if self.driver == "psycopg":
            cur = await self.conn.execute(self._sql_upsert(), (row, value))
            return cur.statusmessage
        return await self.conn.execute(self._sql_upsert(), row, value)

    async def _execute_sql(self, sql: str) -> str:
        if self.driver == "psycopg":
            cur = await self.conn.execute(sql)
            return cur.statusmessage
        return await self.conn.execute(sql)

    async def _cm_write(self, row: str, value: int, *, phase_prefix: str = "") -> OpResult:
        holder = self.holder
        tx = self.conn.transaction()
        self.tx = tx
        # The backend pid is deliberately not part of the judged observation:
        # calibration runs use fresh scenarios/connections, so a pid would make
        # every pre/post pair differ for a harness reason.
        out: dict = {}
        try:
            holder["phase"] = phase_prefix + "enter"
            await tx.__aenter__()
        except BaseException as exc:
            self.diag["enter_exc"] = err_text(exc)
            self.diag["after_enter_failure"] = self._client_state()
            add_event(self.events, "cm_enter_failed", error=err_text(exc),
                      client=self._client_state())
            raise
        try:
            holder["phase"] = phase_prefix + "body"
            out["write"] = await self._execute_upsert(row, value)
        except BaseException as exc:
            out["body_exc"] = err_text(exc)
            self.diag["body_exc"] = err_text(exc)
            self.diag["after_body_exc"] = self._client_state()
            add_event(self.events, "cm_body_failed", error=err_text(exc),
                      client=self._client_state())
            holder["phase"] = phase_prefix + "exit_rollback"
            try:
                await tx.__aexit__(type(exc), exc, exc.__traceback__)
            except BaseException as cleanup:
                self.diag["rollback_cleanup_exc"] = err_text(cleanup)
                self.diag["after_rollback_cleanup"] = self._client_state()
                add_event(self.events, "cm_rollback_cleanup_failed",
                          error=err_text(cleanup), client=self._client_state())
                raise
            self.diag["after_body_rollback"] = self._client_state()
            add_event(self.events, "cm_body_rolled_back",
                      client=self._client_state())
            raise
        holder["phase"] = phase_prefix + "exit_commit"
        try:
            await tx.__aexit__(None, None, None)
        except BaseException as cleanup:
            self.diag["commit_cleanup_exc"] = err_text(cleanup)
            self.diag["after_commit_cleanup"] = self._client_state()
            add_event(self.events, "cm_commit_cleanup_failed",
                      error=err_text(cleanup), client=self._client_state())
            raise
        self.diag["after_commit"] = self._client_state()
        add_event(self.events, "cm_committed", client=self._client_state())
        out["status_after"] = self._client_state()
        return OpResult(True, "commit", out)

    # ---- recovery ----------------------------------------------------
    async def _recover(self) -> dict:
        """Execute the caller-side recovery selected by --recovery."""
        if self.recovery == "none":
            return {"mode": "none"}
        if self.recovery_done:
            return {"mode": self.recovery, "already": True}
        self.recovery_done = True
        before = self._client_state()
        add_event(self.events, "recovery_start", mode=self.recovery, client=before)
        if self.recovery == "rollback":
            if self.driver == "psycopg":
                res = await bound(self.conn.rollback())
            else:
                if self.conn.is_in_transaction():
                    res = await bound(self.conn.execute("ROLLBACK"))
                else:
                    res = ("ok", "skipped-idle")
            after = self._client_state()
            add_event(self.events, "recovery_rollback", result=list(res),
                      client=after)
            return {"mode": "rollback", "before": before,
                    "result": res, "after": after}
        if self.recovery == "raw_rollback":
            res = await bound(self._execute_sql("ROLLBACK"))
            after = self._client_state()
            add_event(self.events, "recovery_raw_rollback", result=list(res),
                      client=after)
            return {"mode": "raw_rollback", "before": before,
                    "result": res, "after": after}
        if self.recovery == "reset":
            if self.driver == "asyncpg":
                res = await bound(self.conn.reset())
            else:
                res = ("err", "unsupported: psycopg has no Connection.reset()")
            after = self._client_state()
            add_event(self.events, "recovery_reset", result=list(res),
                      client=after)
            return {"mode": "reset", "before": before,
                    "result": res, "after": after}
        if self.recovery == "reconnect":
            old = self.pid
            await close_quiet(self.conn.close())
            self.conn = must_ok("reconnect", await bound(
                self._tested_connect(self.proxy_port)
            ))
            self.pid = await self._conn_pid()
            self.diag["reconnected"] = {"old_pid": old, "new_pid": self.pid}
            add_event(self.events, "recovery_reconnect", old_pid=old,
                      new_pid=self.pid)
            return {"mode": "reconnect", "ok": True}
        raise RuntimeError(f"unknown recovery {self.recovery!r}")

    async def _retry_cm(self, row: str, value: int) -> OpResult:
        recovery = await self._recover()
        res = await self._cm_write(row, value, phase_prefix="retry_")
        if not res.ok:
            res.value = {"recovery": recovery, "result": res.value}
            return res
        res.value = {"recovery": recovery, "result": res.value}
        return res

    # ---- Scenario interface -----------------------------------------
    async def target(self):
        self.cut_meta = None
        return await self._cm_write(TARGET_ROW, 1)

    async def same(self):
        return await self._retry_cm(TARGET_ROW, 1)

    async def different(self):
        return await self._retry_cm(B3_ROW, 3)

    async def survivor(self):
        recovery = await self._recover()
        write = await bound(self._cm_write(SURVIVOR_ROW, 7, phase_prefix="survivor_"))
        own = None
        if write[0] == "ok":
            own = await bound(self._read_survivor())
        state = self._client_state()
        # The recovery diagnostics (before/after server state, pids) are not
        # part of the survivor's behaviour: keeping them in the judged value
        # would make pre/post differ merely because the repair path ran.
        add_event(self.events, "survivor_done", write=list(write),
                  own=own, client=state, recovery=recovery)
        return {"recovery": {"mode": recovery.get("mode", "none")},
                "write": write, "own": own, "client": state}

    async def _read_survivor(self):
        if self.driver == "psycopg":
            cur = await self.conn.execute(
                f"SELECT v FROM {self.table} WHERE k = %s", (SURVIVOR_ROW,)
            )
            row = await cur.fetchone()
            return None if row is None else row[0]
        return await self.conn.fetchval(
            f"SELECT v FROM {self.table} WHERE k = $1", SURVIVOR_ROW
        )

    async def observe(self):
        try:
            rows = await self._observer_rows()
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            add_event(self.events, "observer_error", error=err_text(exc))
            raise RuntimeError(f"observer read failed: {err_text(exc)}") from exc
        add_event(self.events, "observer_read", rows=rows)
        return {"rows": rows, "error": None}

    async def close(self) -> None:
        if self.conn is not None:
            await close_quiet(self.conn.close())
        if self.observer is not None:
            await close_quiet(self.observer.close())
        if self.proxy is not None:
            await close_quiet(self.proxy.close())

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return sanitize({"survivor": surv, "observer": obs}, self.table)


# ---------------------------------------------------------------------
# probe: phase map, client/server state, recovery, retry
# ---------------------------------------------------------------------
async def _probe_once(factory, k: int, double: bool, recovery: str) -> None:
    sc = factory()
    sc.recovery = recovery
    try:
        await asyncio.wait_for(sc.setup(), 12)
        started = time.monotonic()
        tr, meta = await _await_phased(
            sc.target(), cut_after=k, timeout=12, holder=sc.holder, double=double
        )
        await asyncio.sleep(0.15)  # let any in-flight delayed reply reach the socket
        server_after_unwind = await bound(sc._server_sample())
        server_after_unwind = (server_after_unwind[1]
                               if isinstance(server_after_unwind, tuple)
                               and server_after_unwind[0] == "ok" else
                               {"error": str(server_after_unwind)})
        client_after_unwind = sc._client_state()
        detail: dict = {
            "driver": sc.driver,
            "k": k,
            "double": double,
            "recovery": recovery,
            "cut_phase": meta["cut_phase"],
            "second_phase": meta["second_phase"],
            "target": {
                "outcome": tr.outcome, "realized": tr.realized,
                "yields": tr.yields, "error": tr.error,
            },
            "client_after_unwind": client_after_unwind,
            "server_after_unwind": server_after_unwind,
            "diag": sc.diag,
        }
        # Recovery is attempted on the same handle, then the official CM is
        # retried: this is where a silently nested savepoint would show up as
        # success with an invisible write.
        try:
            detail["recovery_result"] = await bound(sc._recover())
        except BaseException as exc:
            detail["recovery_result"] = err_text(exc)
        try:
            retry = await sc._cm_write(RETRY_ROW, 5, phase_prefix="retry_")
            detail["retry"] = {"ok": retry.ok, "stage": retry.stage,
                               "value": retry.value, "error": retry.error}
        except BaseException as exc:
            detail["retry"] = err_text(exc)
        detail["client_after_retry"] = sc._client_state()
        detail["observer_rows"] = await bound(sc._observer_rows())
        srv_retry = await bound(sc._server_sample())
        detail["server_after_retry"] = (srv_retry[1]
                                        if isinstance(srv_retry, tuple)
                                        and srv_retry[0] == "ok" else
                                        {"error": str(srv_retry)})
        detail["events"] = sc.events
        print("TXCUT " + json.dumps(sanitize(detail, sc.table), default=str,
                                    sort_keys=True), flush=True)
        print(f"TXCUT-SUMMARY k={k} phase={meta['cut_phase']} "
              f"second={meta['second_phase']} "
              f"client={normalize(client_after_unwind)} "
              f"server_state={server_after_unwind.get('state')} "
              f"retry={detail['retry'] if isinstance(detail['retry'], str) else detail['retry'].get('ok')} "
              f"rows={detail['observer_rows']} "
              f"seconds={time.monotonic() - started:.2f}", flush=True)
    finally:
        await close_quiet(sc.close())


async def probe_run(factory, ks: list[int], double: bool, recovery: str) -> None:
    for k in ks:
        await _probe_once(factory, k, double, recovery)


async def count_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        tr = await _await_target(sc.target(), None, 12)
        print(f"COUNT driver={sc.driver} outcome={tr.outcome} "
              f"yields={tr.yields} result={tr.result!r} "
              f"diag={sc.diag!r}", flush=True)
    finally:
        await close_quiet(sc.close())


# ---------------------------------------------------------------------
def build_factory(args):
    return lambda: CmScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument("--driver", default="psycopg", choices=["psycopg", "asyncpg"])
    parser.add_argument("--count", action="store_true",
                        help="print uncancelled target yields")
    parser.add_argument("--probe-phase", default=None,
                        help="comma-separated cut indices for the phase probe")
    parser.add_argument("--fix-enter", action="store_true",
                        help="install the experimental cancelled-__aenter__ repair")
    parser.add_argument("--double-cancel", action="store_true",
                        help="also cancel at the first suspension after the cut")
    parser.add_argument("--recovery", default="none",
                        choices=["none", "rollback", "raw_rollback", "reset",
                                 "reconnect"])
    args = parser.parse_args()

    print(f"case=txphase driver={args.driver} python={sys.version.split()[0]} "
          f"host={args.host} port={args.port} db={args.dbname} "
          f"delay={DELAY} bound={BOUND} recovery={args.recovery}", flush=True)
    if args.fix_enter:
        if args.driver == "psycopg":
            install_psycopg_enter_fix()
        else:
            install_asyncpg_enter_fix()
        print("FIX-VARIANT installed: cancelled __aenter__ repair "
              f"({args.driver})", flush=True)
    factory = build_factory(args)

    if args.count:
        await count_run(factory)
        return
    if args.probe_phase:
        ks = [int(x) for x in args.probe_phase.split(",") if x.strip()]
        await probe_run(factory, ks, args.double_cancel, args.recovery)
        return

    timeout = 12.0
    reports = []
    for level in [x.strip() for x in args.levels.split(",") if x.strip()]:
        if level == "B1":
            r = await level_b1(f"txphase-{args.driver}", factory,
                               reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(f"txphase-{args.driver}", factory, level="B2",
                                  retry="same", reps=args.reps, timeout=timeout)
        elif level == "B3":
            r = await level_retry(f"txphase-{args.driver}", factory, level="B3",
                                  retry="different", reps=args.reps,
                                  timeout=timeout)
        elif level == "M1":
            r = await explore_observed(f"txphase-{args.driver}", factory,
                                       reps=args.reps, timeout=timeout)
        else:
            raise SystemExit(f"unknown level {level}")
        print_report(r)
        print(compact(r), flush=True)
        reports.append(r)

    print("=== FINAL ===", flush=True)
    for r in reports:
        print(f"driver={args.driver} recovery={args.recovery} {compact(r)}",
              flush=True)
        print(f"violations_by_plan={[t.plan for t in r.violations()]}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
