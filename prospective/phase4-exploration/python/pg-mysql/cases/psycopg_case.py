"""psycopg 3.3.6 + psycopg-pool 3.3.3 cancellation cases.

Two reuse units, same skeleton:

  pool    AsyncConnectionPool(min_size=1, max_size=1), autocommit through
          `configure`; target checks out a connection with `getconn()`, runs
          BEGIN / upsert(target) / COMMIT, and returns it with `putconn()`.
  direct  one long-lived AsyncConnection (autocommit=True); the target only
          borrows it, the survivor uses the same object.

Target = explicit transaction that writes the target row and commits
(BEGIN + write + COMMIT are three awaits in the same target coroutine).
Survivor = same pool/handle, upserts the survivor row in the same table and
reads it back on the same connection.  Observer = separate direct connection,
not through the pool and not through the delay proxy.

Pending mechanism for both modes: `DelayProxy` (retro `cases/common.py`,
50 ms on every server->client chunk) wrapping only the tested connection.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import time

import psycopg
import psycopg_pool

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
    observation,
    op_failure,
    sanitize,
    unique_table,
)
from dropwise_py import (  # noqa: E402
    Scenario,
    explore_observed,
    level_b1,
    level_retry,
    print_report,
)
from dropwise_py.engine import OpResult, _await_target  # noqa: E402

TARGET_ROW = "target"
SURVIVOR_ROW = "survivor"
B3_ROW = "b3"

TX_NAMES = {0: "IDLE", 1: "ACTIVE", 2: "INTRANS", 3: "INERROR", 4: "UNKNOWN"}


def tx_name(value) -> str:
    try:
        return TX_NAMES.get(int(value), str(value))
    except Exception:
        return str(value)


def conninfo(host: str, port: int, user: str, password: str, dbname: str) -> str:
    return (
        f"host={host} port={port} user={user} password={password} "
        f"dbname={dbname} connect_timeout=5"
    )


class _StageFailure(Exception):
    """Internal: abort a transaction-block stage without swallowing cancel."""

    def __init__(self, result: OpResult) -> None:
        super().__init__(result.error)
        self.result = result


class PsycopgScenario(Scenario):
    def __init__(self, args) -> None:
        self.args = args
        self.mode = args.mode
        self.table = unique_table("dw_r3_pg")
        self.proxy: DelayProxy | None = None
        self.pool: psycopg_pool.AsyncConnectionPool | None = None
        self.conn: psycopg.AsyncConnection | None = None
        self.observer: psycopg.AsyncConnection | None = None
        self.target_style = getattr(args, "target_style", "manual")
        self.sample_server = False
        self.events: list[dict] = []
        self.dsn_direct: str | None = None
        self.dsn_proxy: str | None = None
        # diagnostics only (never part of the judged observation)
        self.diag: dict = {}

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        self.dsn_direct = conninfo(a.host, a.port, a.user, a.password, a.dbname)
        self.dsn_proxy = conninfo(a.host, client_port, a.user, a.password, a.dbname)

        # observer first, direct to the server (no proxy, no tested pool)
        obs = await bound(psycopg.AsyncConnection.connect(self.dsn_direct, autocommit=True))
        self.observer = must_ok("observer connect", obs)
        created = await bound(
            self.observer.execute(
                f"CREATE TABLE {self.table} (k text PRIMARY KEY, v integer NOT NULL)"
            )
        )
        must_ok("create table", created)
        ping = await bound(self.observer.execute("SELECT 1"))
        must_ok("observer ping", ping)

        if self.mode == "pool":
            await self._open_pool()
            self.diag["setup_pid"] = await self._pid_pool()
        else:
            await self._open_direct()
            self.diag["setup_pid"] = self.conn.info.backend_pid
        self.diag["setup_stats"] = self._stats()
        # A failed baseline must never become a valid calibration: exercise the
        # exact survivor + independent-observer path once, hard-fail if broken.
        await self._preflight()

    async def _open_pool(self) -> None:
        async def configure(conn):
            await conn.set_autocommit(True)

        pool = psycopg_pool.AsyncConnectionPool(
            self.dsn_proxy,
            min_size=1,
            max_size=1,
            open=False,
            configure=configure,
            timeout=BOUND,
        )
        must_ok("pool open", await bound(pool.open(wait=True, timeout=BOUND)))
        self.pool = pool
        must_ok("pool warm-up", await bound(self._pool_exec("SELECT 1")))

    async def _open_direct(self) -> None:
        conn = must_ok(
            "tested connect",
            await bound(psycopg.AsyncConnection.connect(self.dsn_proxy, autocommit=True)),
        )
        self.conn = conn
        must_ok("direct warm-up", await bound(self._exec(conn, "SELECT 1")))

    async def _preflight(self) -> None:
        surv = await bound(self.survivor())
        if surv[0] != "ok":
            raise RuntimeError(f"survivor preflight raised: {surv!r}")
        write = surv[1].get("write")
        if not isinstance(write, tuple) or write[0] != "ok":
            raise RuntimeError(f"survivor preflight write failed: {surv[1]!r}")
        obs = await bound(self.observe())
        if obs[0] != "ok":
            raise RuntimeError(f"observer preflight failed: {obs!r}")
        if obs[1].get("rows") != [["survivor", 7]]:
            raise RuntimeError(f"survivor preflight not observed: {obs[1]!r}")
        # Remove the preflight row again: seeding it would hide a later
        # uncommitted survivor write from the independent observer.
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            raise RuntimeError(f"preflight cleanup acquire failed: {acq!r}")
        conn = acq[1]
        try:
            deleted = await bound(
                self._exec(conn, f"DELETE FROM {self.table} WHERE k = %s",
                           (SURVIVOR_ROW,))
            )
            must_ok("preflight cleanup delete", deleted)
        finally:
            must_ok("preflight cleanup release", await bound(self._release(conn)))
        empty = await bound(self.observe())
        if empty[0] != "ok" or empty[1].get("rows") != []:
            raise RuntimeError(f"preflight cleanup not observed: {empty!r}")
        self.diag["preflight"] = {"survivor": surv[1], "rows": obs[1]["rows"],
                                  "cleanup_rows": empty[1]["rows"]}
        self.events.clear()

    async def _pool_exec(self, sql: str, params=None):
        conn = await self.pool.getconn(timeout=BOUND)
        try:
            return await self._exec(conn, sql, params)
        finally:
            await self.pool.putconn(conn)

    async def _exec(self, conn, sql: str, params=None):
        cur = await conn.execute(sql, params)
        return cur.statusmessage

    async def _upsert(self, conn, row: str, value: int):
        return await self._exec(
            conn,
            f"INSERT INTO {self.table} (k, v) VALUES (%s, %s) "
            "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
            (row, value),
        )

    async def _pid_pool(self) -> int | None:
        try:
            conn = await self.pool.getconn(timeout=BOUND)
        except BaseException as exc:
            self.diag["pid_pool_error"] = err_text(exc)
            return None
        try:
            return conn.info.backend_pid
        finally:
            await self.pool.putconn(conn)

    def _stats(self) -> dict | None:
        if self.pool is None:
            return None
        try:
            return dict(self.pool.get_stats())
        except BaseException as exc:
            return {"error": type(exc).__name__}

    async def _acquire(self):
        if self.mode == "pool":
            return await self.pool.getconn(timeout=BOUND)
        return self.conn

    async def _release(self, conn) -> None:
        if self.mode == "pool" and conn is not None:
            await self.pool.putconn(conn)

    async def _server_sample(self, pid: int) -> dict:
        """Independent server-side sample; *not* synchronous with client state."""
        try:
            cur = await asyncio.wait_for(
                self.observer.execute(
                    "SELECT pid, state, backend_xid::text, wait_event_type, "
                    "wait_event, left(query, 160) FROM pg_stat_activity "
                    "WHERE pid = %s",
                    (pid,),
                ),
                BOUND,
            )
            row = await asyncio.wait_for(cur.fetchone(), BOUND)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}
        if row is None:
            return {"pid": pid, "present": False}
        return {
            "pid": row[0], "state": row[1], "backend_xid": row[2],
            "wait_event_type": row[3], "wait_event": row[4], "query": row[5],
        }

    # ---- target ------------------------------------------------------
    async def _tx_write(self, row: str, value: int, *, style: str | None = None):
        """BEGIN / upsert(row, value) / COMMIT, on one checked-out conn.

        Every stage is checked.  A failed acquire/BEGIN/INSERT stops before
        COMMIT and is returned as ``OpResult(ok=False, stage=...)``; a release
        failure is surfaced the same way.  Cancellation is never converted to
        a value: it propagates (after the bounded release attempt).
        """
        style = style or self.target_style
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return op_failure("acquire", acq)
        conn = acq[1]
        out: dict = {
            "style": style,
            "backend_pid": conn.info.backend_pid,
            "status_after_acquire": tx_name(conn.info.transaction_status),
        }
        add_event(
            self.events, "target_acquire", pid=out["backend_pid"],
            status=out["status_after_acquire"], style=style,
        )
        stages = self.diag.setdefault("stages", [])
        failure: OpResult | None = None
        release: tuple | None = None
        try:
            if style == "cm":
                try:
                    async with conn.transaction():
                        res = await bound(self._upsert(conn, row, value))
                        out["insert"] = res
                        add_event(self.events, "target_stage", stage="insert",
                                  result=list(res))
                        if res[0] == "ok":
                            stages.append("insert")
                        else:
                            raise _StageFailure(op_failure("insert", res, out))
                except _StageFailure as exc:
                    failure = exc.result
                    add_event(self.events, "target_stage_failed",
                              stage=failure.stage)
                except asyncio.CancelledError:
                    self.diag["cm_cancel_status"] = tx_name(
                        conn.info.transaction_status
                    )
                    add_event(self.events, "target_cm_cancelled",
                              status=self.diag["cm_cancel_status"])
                    raise
                except BaseException as exc:
                    failure = OpResult(False, "cm", out, err_text(exc))
                    add_event(self.events, "target_cm_error", error=err_text(exc))
                else:
                    stages.append("commit")
            else:
                operations = (
                    ("begin", lambda: self._exec(conn, "BEGIN")),
                    ("insert", lambda: self._upsert(conn, row, value)),
                    ("commit", lambda: self._exec(conn, "COMMIT")),
                )
                for stage, make in operations:
                    res = await bound(make())
                    out[stage] = res
                    add_event(self.events, "target_stage", stage=stage,
                              result=list(res))
                    if res[0] == "ok":
                        stages.append(stage)
                        continue
                    failure = op_failure(stage, res, out)
                    add_event(self.events, "target_stage_failed", stage=stage,
                              result=list(res))
                    break  # never continue to COMMIT after a failed BEGIN/INSERT
        except asyncio.CancelledError:
            add_event(self.events, "target_cancelled_inside",
                      status=tx_name(conn.info.transaction_status),
                      stages=list(stages))
            raise
        finally:
            # state handed to the next user, recorded before any reset
            status = tx_name(conn.info.transaction_status)
            self.diag["after_target"] = {
                "pid": conn.info.backend_pid,
                "status": status,
                "pool_before_release": self._stats(),
            }
            out["status_before_release"] = status
            add_event(self.events, "target_before_release",
                      pid=conn.info.backend_pid, status=status,
                      stages=list(stages))
            # Release is the first await in cleanup: a diagnostic must never
            # run before it, or a second cancellation could skip the release
            # and strand a pool checkout.
            release = await bound(self._release(conn))
            out["release"] = release
            if self.sample_server:
                sample = await bound(self._server_sample(conn.info.backend_pid))
                out["server_after_release"] = sample
                add_event(self.events, "server_after_release", sample=sample)
            self.diag["after_release"] = {
                "status": tx_name(conn.info.transaction_status),
                "pool": self._stats(),
            }
            add_event(self.events, "target_released", release=list(release),
                      status=tx_name(conn.info.transaction_status),
                      pool=self._stats())
            if failure is None and release[0] != "ok":
                failure = op_failure("release", release, out)
        if failure is not None:
            return failure
        return OpResult(True, "commit", out)

    async def _cm_write(self, row: str, value: int):
        """Retry transaction through the documented transaction block."""
        return await self._tx_write(row, value, style="cm")

    async def target(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def same(self):
        return await self._tx_write(TARGET_ROW, 1)

    async def different(self):
        return await self._tx_write(B3_ROW, 3)

    # ---- boundary / recovery diagnostics -----------------------------
    async def recover_rollback(self) -> dict:
        """Explicit caller recovery: inspect, rollback, then reuse."""
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            return {"acquire": acq}
        conn = acq[1]
        before = tx_name(conn.info.transaction_status)
        add_event(self.events, "recovery_acquire", pid=conn.info.backend_pid,
                  status=before)
        rb = await bound(conn.rollback())
        after = tx_name(conn.info.transaction_status)
        add_event(self.events, "recovery_rollback", result=list(rb),
                  status=after)
        rel = await bound(self._release(conn))
        add_event(self.events, "recovery_release", result=list(rel))
        return {"status_before": before, "rollback": rb, "status_after": after,
                "release": rel}

    async def reconnect_handle(self) -> dict:
        """Control: discard the handle/pool, reconnect, then reuse."""
        old_pid = self.diag.get("setup_pid")
        if self.mode == "pool":
            if self.pool is not None:
                await close_quiet(self.pool.close(timeout=1.0))
                self.pool = None
            await self._open_pool()
            new_pid = await self._pid_pool()
        else:
            if self.conn is not None:
                old_pid = self.conn.info.backend_pid
                await close_quiet(self.conn.close())
                self.conn = None
            await self._open_direct()
            new_pid = self.conn.info.backend_pid
        add_event(self.events, "reconnected", old_pid=old_pid, new_pid=new_pid)
        return {"old_pid": old_pid, "new_pid": new_pid}

    # ---- survivor / observer ----------------------------------------
    async def survivor(self):
        acq = await bound(self._acquire())
        if acq[0] != "ok":
            raise RuntimeError(f"survivor acquire failed: {acq!r}")
        conn = acq[1]
        acquire_status = tx_name(conn.info.transaction_status)
        self.diag["survivor_start"] = {
            "pid": conn.info.backend_pid,
            "status": acquire_status,
            "pool": self._stats(),
        }
        add_event(self.events, "survivor_acquire", pid=conn.info.backend_pid,
                  status=acquire_status, pool=self._stats())
        if self.sample_server:
            sample = await bound(self._server_sample(conn.info.backend_pid))
            add_event(self.events, "server_after_survivor_acquire", sample=sample)
        write = await bound(self._upsert(conn, SURVIVOR_ROW, 7))
        add_event(self.events, "survivor_write", result=list(write),
                  status=tx_name(conn.info.transaction_status))
        if self.sample_server:
            sample = await bound(self._server_sample(conn.info.backend_pid))
            add_event(self.events, "server_after_survivor_write", sample=sample)
        own = None
        if write[0] != "timeout":
            own = await bound(self._read_scalar(conn, SURVIVOR_ROW))
        release = await bound(self._release(conn))
        status_end = tx_name(conn.info.transaction_status)
        self.diag["survivor_end"] = {
            "status": status_end,
            "pool": self._stats(),
        }
        add_event(self.events, "survivor_release", result=list(release),
                  status=status_end)
        return {"in_tx": acquire_status, "write": write, "own": own,
                "release": release}

    async def _read_scalar(self, conn, row: str):
        cur = await conn.execute(
            f"SELECT v FROM {self.table} WHERE k = %s", (row,)
        )
        got = await cur.fetchone()
        return None if got is None else got[0]

    async def observe(self):
        try:
            cur = await self.observer.execute(
                f"SELECT k, v FROM {self.table} ORDER BY k"
            )
            rows = await cur.fetchall()
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            add_event(self.events, "observer_error", error=err_text(exc))
            raise RuntimeError(f"observer read failed: {err_text(exc)}") from exc
        data = [[k, v] for k, v in rows]
        add_event(self.events, "observer_read", rows=data)
        return {"rows": data, "error": None}

    # ---- cleanup -----------------------------------------------------
    async def close(self) -> None:
        if self.pool is not None:
            await close_quiet(self.pool.close(timeout=1.0))
        if self.conn is not None:
            await close_quiet(self.conn.close())
        if self.observer is not None:
            await close_quiet(self.observer.close())
        if self.proxy is not None:
            await close_quiet(self.proxy.close())

    async def judged(self) -> dict:
        surv = await bound(self.survivor())
        obs = await bound(self.observe())
        return observation(surv, obs, self.table)


# ---------------------------------------------------------------------
# diagnostics: reuse, pre/post, per-yield detail
# ---------------------------------------------------------------------
async def diag_run(factory) -> None:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), 12)
        pids = []
        for _ in range(3):
            v = await bound(sc._pid_pool() if sc.mode == "pool" else _pid(sc))
            pids.append(v)
        print(f"DIAG reuse pids={pids} stats={sc._stats()}", flush=True)

        pre = await sc.judged()
        print(f"DIAG pre obs={pre!r}", flush=True)
        tr = await _await_target(sc.target(), None, 12)
        print(
            f"DIAG post target={tr.outcome}/{tr.yields} result={tr.result!r} "
            f"diag={sc.diag!r}",
            flush=True,
        )
        post = await sc.judged()
        print(f"DIAG post obs={post!r}", flush=True)
        print(f"DIAG pre==post {pre == post}", flush=True)
    finally:
        await close_quiet(sc.close())


async def _pid(sc):
    return sc.conn.info.backend_pid


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
                f"yields={tr.yields} err={tr.error!r} "
                f"diag={sc.diag!r}\n"
                f"        survivor={surv!r}\n"
                f"        observer={obs!r} "
                f"seconds={time.monotonic() - started:.2f}",
                flush=True,
            )
        finally:
            await close_quiet(sc.close())


# ---------------------------------------------------------------------
async def _contrast_once(factory, variant: str, k: int) -> None:
    sc = factory()
    sc.sample_server = True
    try:
        await asyncio.wait_for(sc.setup(), 12)
        if variant == "cm_target":
            sc.target_style = "cm"
        tr = await _await_target(sc.target(), k, 12)
        add_event(
            sc.events, "cut_returned", outcome=tr.outcome, yields=tr.yields,
            realized=tr.realized, seconds=round(tr.seconds, 6),
        )
        detail: dict = {
            "case": "psycopg",
            "mode": sc.mode,
            "variant": variant,
            "k": k,
            "target": {
                "outcome": tr.outcome, "yields": tr.yields,
                "realized": tr.realized, "error": tr.error,
            },
            "after_target": sc.diag.get("after_target"),
            "after_release": sc.diag.get("after_release"),
        }
        if variant == "rollback":
            detail["recovery"] = await sc.recover_rollback()
        elif variant == "cm_retry":
            detail["cm_retry"] = await sc._cm_write(B3_ROW, 3)
            add_event(sc.events, "cm_retry_result",
                      result=detail["cm_retry"])
        elif variant == "reconnect":
            detail["reconnect"] = await sc.reconnect_handle()
        detail["survivor"] = await bound(sc.survivor(), 8)
        detail["observer"] = await bound(sc.observe(), 8)
        detail["events"] = sc.events
        print("BOUNDARY " + json.dumps(sanitize(detail, sc.table),
                                        default=str, sort_keys=True), flush=True)
    finally:
        await close_quiet(sc.close())


async def contrast_run(factory, plans: list[int]) -> None:
    for variant in ("manual", "rollback", "cm_retry", "cm_target", "reconnect"):
        for k in plans:
            await _contrast_once(factory, variant, k)


# ---------------------------------------------------------------------
def build_factory(args):
    return lambda: PsycopgScenario(args)


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument(
        "--diag", action="store_true", help="reuse + pre/post calibration details"
    )
    parser.add_argument("--contrast", action="store_true",
                        help="boundary/recovery variants after a cut")
    parser.add_argument("--plans", default=None,
                        help="comma-separated plan indices for --contrast")
    args = parser.parse_args()

    print(
        f"case=psycopg mode={args.mode} psycopg={psycopg.__version__} "
        f"psycopg_pool={psycopg_pool.__version__} python={__import__('sys').version.split()[0]} "
        f"host={args.host} port={args.port} db={args.dbname} round={args.round} "
        f"delay={DELAY} bound={BOUND}",
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
    if args.contrast:
        plans = ([int(x) for x in args.plans.split(",") if x.strip()]
                 if args.plans else [1, 2, 3, 4, 5, 6])
        await contrast_run(factory, plans)
        return

    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    timeout = 12.0
    reports = []
    for level in levels:
        if level == "B1":
            r = await level_b1("psycopg", factory, reps=args.reps, timeout=timeout)
        elif level == "B2":
            r = await level_retry(
                "psycopg", factory, level="B2", retry="same",
                reps=args.reps, timeout=timeout,
            )
        elif level == "B3":
            r = await level_retry(
                "psycopg", factory, level="B3", retry="different",
                reps=args.reps, timeout=timeout,
            )
        elif level == "M1":
            r = await explore_observed(
                "psycopg", factory, reps=args.reps, timeout=timeout
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
