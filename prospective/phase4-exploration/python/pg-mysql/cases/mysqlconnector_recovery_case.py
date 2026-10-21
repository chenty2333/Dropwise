"""mysql-connector-python 26.7.0 async direct: recovery contrast after cancel.

Round 3b (`RESULTS-FRAGMENT-round3b.md` section 5) showed that cancelling an
in-flight `start_transaction()` / parameterized `REPLACE` / `commit()` leaves an
unread reply in the aio connection and the next command consumes it.  This
module asks the next question: **can an official recovery call return OK and
still leave the client unusable or the server transaction open?**

One scenario instance runs exactly one (cut, strategy) pair; state is mutated
by the cancelled target, so the runner starts a fresh process per pair and a
fresh scenario per rep.  Strategy is one of:

    rollback        await conn.rollback()
    reset_session   await conn.reset_session()   (internally cmd_reset_connection)
    close_reconnect await conn.close(); aio.connect(...) again through the proxy

Order of records per rep:

1. cancelled target (same `start_transaction`/`REPLACE`/`commit` target as
   round 3b, same `_await_target` cut counting);
2. local state on the tested handle (cid / in_transaction / unread_result /
   is_socket_connected -- all local, no round trip, so the stream is not
   touched);
3. independent observer sample *before* recovery (table rows, innodb_trx,
   performance_schema.events_transactions_current, processlist);
4. the bounded (2 s) recovery call;
5. local state again;
6. survivor `REPLACE(survivor,7)` + same-handle `SELECT` (bounded 2 s each);
7. independent observer sample after the survivor, including the MySQL general
   log (`log_output=TABLE`) filtered to the tested connection id(s), proving
   which statements actually reached the server and in what order.

The observer is a separate direct `mysql.connector.aio` connection (no proxy).
Every operation is bounded; a hang becomes a value, not a dead trial.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import time

import mysql.connector
import mysql.connector.aio as aio

from .common import (  # noqa: E402  (also puts research/python-retro on sys.path)
    BOUND,
    DELAY,
    DelayProxy,
    add_db_args,
    bound,
    close_quiet,
    unique_table,
)
from dropwise_py.engine import _await_target  # noqa: E402

TARGET_ROW = "target"
SURVIVOR_ROW = "survivor"
STRATEGIES = ("rollback", "reset_session", "close_reconnect")


class RecoveryScenario:
    def __init__(self, args, cut: int, strategy: str, rep: int) -> None:
        self.args = args
        self.cut = cut
        self.strategy = strategy
        self.rep = rep
        self.table = unique_table("dw_r4_mcrec")
        self.proxy: DelayProxy | None = None
        self.conn = None
        self.observer = None
        self.proxy_cfg: dict = {}
        self.diag: dict = {}
        self.cids: list[int] = []

    # ---- small helpers ----------------------------------------------
    def _state(self) -> dict:
        """Client-reported state only; local attributes, no round trip."""
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

    async def _exec_conn(self, conn, sql: str, params=()):
        cur = await conn.cursor()
        try:
            await cur.execute(sql, params)
            if cur.with_rows:
                await cur.fetchall()
            return cur.rowcount
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _fetch(self, conn, sql: str, params=()):
        cur = await conn.cursor()
        try:
            await cur.execute(sql, params)
            return [list(r) for r in await cur.fetchall()]
        finally:
            with contextlib.suppress(BaseException):
                await cur.close()

    async def _connect(self, cfg: dict):
        return await aio.connect(**cfg, autocommit=True, connection_timeout=5)

    # ---- setup -------------------------------------------------------
    async def setup(self) -> None:
        a = self.args
        self.proxy = DelayProxy(a.host, a.port, delay=DELAY)
        client_port = await asyncio.wait_for(self.proxy.start(), BOUND)
        common_cfg = dict(
            host=a.host,
            port=a.port,
            user=a.user,
            password=a.password,
            database=a.dbname,
        )
        self.proxy_cfg = dict(common_cfg, port=client_port)

        obs = await bound(self._connect(common_cfg))
        if obs[0] != "ok":
            raise RuntimeError(f"observer connect failed: {obs}")
        self.observer = obs[1]

        created = await bound(
            self._exec_conn(
                self.observer,
                f"CREATE TABLE {self.table} (k VARCHAR(32) PRIMARY KEY, v INT NOT NULL)",
            )
        )
        if created[0] != "ok":
            raise RuntimeError(f"create table failed: {created}")

        # Fresh general log per scenario instance.  OFF is required for
        # TRUNCATE; the tested connection is opened after it is back ON so its
        # post-connection statements are visible too.
        glog = {}
        for label, sql in (
            ("log_output", "SET GLOBAL log_output='TABLE'"),
            ("off", "SET GLOBAL general_log=OFF"),
            ("truncate", "TRUNCATE TABLE mysql.general_log"),
            ("on", "SET GLOBAL general_log=ON"),
        ):
            res = await bound(self._exec_conn(self.observer, sql))
            glog[label] = res
            if res[0] != "ok":
                raise RuntimeError(f"general log {label} failed: {res}")
        self.diag["general_log_setup"] = glog

        conn = await bound(self._connect(self.proxy_cfg))
        if conn[0] != "ok":
            raise RuntimeError(f"tested connect failed: {conn}")
        self.conn = conn[1]
        warm = await bound(self._exec_conn(self.conn, "SELECT 1"))
        if warm[0] != "ok":
            raise RuntimeError(f"warm-up failed: {warm}")
        self.diag["setup"] = self._state()
        ac = await bound(self.conn.get_autocommit())
        connected = await bound(self.conn.is_connected())
        self.diag["setup_autocommit"] = ac
        self.diag["setup_connected"] = connected
        if ac[0] != "ok" or ac[1] is not True:
            raise RuntimeError(f"setup autocommit check failed: {ac}")
        if connected[0] != "ok" or connected[1] is not True:
            raise RuntimeError(f"setup connectivity check failed: {connected}")
        if self.conn.connection_id:
            self.cids.append(int(self.conn.connection_id))
        ver = await bound(self._fetch(self.observer, "SELECT VERSION()"))
        self.diag["server_version"] = ver

    # ---- target ------------------------------------------------------
    async def _tx_write(self):
        """start_transaction / REPLACE(target,1) / commit, stop at first failure."""
        out = {
            "cid": getattr(self.conn, "connection_id", None),
            "in_tx_before": bool(self.conn.in_transaction),
            "start_transaction": None,
            "insert": None,
            "commit": None,
        }
        stages: list[str] = self.diag.setdefault("stages", [])
        for name in ("start_transaction", "insert", "commit"):
            # Build each coroutine lazily: a target cancelled at an earlier
            # stage must not leave later coroutines unawaited.
            if name == "start_transaction":
                coro = self.conn.start_transaction()
            elif name == "insert":
                coro = self._exec_conn(
                    self.conn,
                    f"REPLACE INTO {self.table} (k, v) VALUES (%s, %s)",
                    (TARGET_ROW, 1),
                )
            else:
                coro = self.conn.commit()
            res = await bound(coro)
            out[name] = res
            if res[0] != "ok":
                raise RuntimeError(f"{name} failed: {res!r}")
            stages.append(name)
        return out

    # ---- observer evidence -------------------------------------------
    async def _observer_evidence(self, include_log: bool) -> dict:
        ev: dict = {}
        rows = await bound(
            self._fetch(self.observer, f"SELECT k, v FROM {self.table} ORDER BY k")
        )
        ev["rows"] = rows

        cids = sorted({int(c) for c in self.cids if c is not None})
        if not cids:
            ev["innodb_trx"] = ("err", "no tested connection id")
            ev["ps_transactions"] = ("err", "no tested connection id")
            ev["processlist"] = ("err", "no tested connection id")
        else:
            in_list = ",".join(str(c) for c in cids)
            trx_count = await bound(
                self._fetch(
                    self.observer,
                    "SELECT COUNT(*) FROM information_schema.innodb_trx",
                )
            )
            trx_rows = await bound(
                self._fetch(
                    self.observer,
                    "SELECT trx_id, trx_state, trx_mysql_thread_id "
                    "FROM information_schema.innodb_trx "
                    f"WHERE trx_mysql_thread_id IN ({in_list})",
                )
            )
            ev["innodb_trx_count"] = trx_count
            ev["innodb_trx_for_tested"] = trx_rows
            ev["ps_transactions"] = await bound(
                self._fetch(
                    self.observer,
                    "SELECT t.PROCESSLIST_ID, e.STATE, e.ACCESS_MODE, "
                    "e.ISOLATION_LEVEL, e.AUTOCOMMIT "
                    "FROM performance_schema.events_transactions_current e "
                    "JOIN performance_schema.threads t ON t.THREAD_ID = e.THREAD_ID "
                    f"WHERE t.PROCESSLIST_ID IN ({in_list})",
                )
            )
            ev["processlist"] = await bound(
                self._fetch(
                    self.observer,
                    "SELECT ID, USER, DB, COMMAND, TIME, STATE, INFO "
                    "FROM information_schema.processlist "
                    f"WHERE ID IN ({in_list})",
                )
            )

        if include_log:
            if cids:
                in_list = ",".join(str(c) for c in cids)
                log = await bound(
                    self._fetch(
                        self.observer,
                        "SELECT event_time, thread_id, command_type, argument "
                        "FROM mysql.general_log "
                        f"WHERE thread_id IN ({in_list}) ORDER BY event_time",
                    )
                )
                if log[0] == "ok":
                    ev["general_log"] = [
                        [
                            str(row[0]),
                            int(row[1]),
                            str(row[2]),
                            (row[3].decode("utf-8", "backslashreplace")
                             if isinstance(row[3], (bytes, bytearray)) else str(row[3]))[:300],
                        ]
                        for row in log[1]
                    ]
                else:
                    ev["general_log"] = log
            else:
                ev["general_log"] = ("err", "no tested connection id")
        return ev

    # ---- one full run ------------------------------------------------
    async def run(self) -> dict:
        rec: dict = {
            "meta": {
                "cut": self.cut,
                "strategy": self.strategy,
                "rep": self.rep,
                "table": self.table,
                "connector": mysql.connector.__version__,
                "connector_file": mysql.connector.__file__,
                "python": __import__("sys").version.split()[0],
                "delay": DELAY,
                "bound": BOUND,
            },
            "setup": None,
            "target": None,
            "after_cancel": None,
            "observer_after_cancel": None,
            "recovery": None,
            "after_recovery": None,
            "survivor": None,
            "observer_after": None,
            "close": None,
        }
        started = time.monotonic()
        close_rec: dict = {}
        try:
            await asyncio.wait_for(self.setup(), 15)
            rec["setup"] = self.diag.get("setup")
            rec["server_version"] = self.diag.get("server_version")

            tr = await _await_target(self._tx_write(), self.cut, 12.0)
            rec["target"] = {
                "outcome": tr.outcome,
                "realized": tr.realized,
                "yields": tr.yields,
                "error": tr.error,
                "seconds": round(tr.seconds, 3),
                "diag": self.diag,
            }
            rec["after_cancel"] = self._state()

            rec["observer_after_cancel"] = await asyncio.wait_for(
                self._observer_evidence(include_log=False), 6
            )

            t0 = time.monotonic()
            if self.strategy == "rollback":
                call = await bound(self.conn.rollback())
                rec["recovery"] = {"strategy": self.strategy, "call": call}
            elif self.strategy == "reset_session":
                call = await bound(self.conn.reset_session())
                rec["recovery"] = {"strategy": self.strategy, "call": call}
            elif self.strategy == "close_reconnect":
                close_res = await bound(self.conn.close())
                old_cid = getattr(self.conn, "connection_id", None)
                self.conn = None
                reconnect = await bound(self._connect(self.proxy_cfg))
                if reconnect[0] == "ok":
                    self.conn = reconnect[1]
                    if self.conn.connection_id:
                        self.cids.append(int(self.conn.connection_id))
                rec["recovery"] = {
                    "strategy": self.strategy,
                    "close": close_res,
                    "old_cid": old_cid,
                    "reconnect": reconnect,
                }
            else:  # pragma: no cover - guarded by CLI choices
                raise RuntimeError(f"unknown strategy {self.strategy}")
            rec["recovery"]["seconds"] = round(time.monotonic() - t0, 3)
            rec["after_recovery"] = self._state()

            if self.conn is None:
                rec["survivor"] = {"error": "no handle after recovery"}
            else:
                state = self._state()
                write = await bound(
                    self._exec_conn(
                        self.conn,
                        f"REPLACE INTO {self.table} (k, v) VALUES (%s, %s)",
                        (SURVIVOR_ROW, 7),
                    )
                )
                own = None
                if write[0] != "timeout":
                    own = await bound(
                        self._fetch(
                            self.conn,
                            f"SELECT v FROM {self.table} WHERE k = %s",
                            (SURVIVOR_ROW,),
                        )
                    )
                rec["survivor"] = {
                    "state": state,
                    "write": write,
                    "own": own,
                    "after": self._state(),
                }

            # Stop logging before the diagnostic reads (the tested connection
            # is idle from here on).
            rec["general_log_off"] = await bound(
                self._exec_conn(self.observer, "SET GLOBAL general_log=OFF")
            )
            rec["observer_after"] = await asyncio.wait_for(
                self._observer_evidence(include_log=True), 8
            )
        except BaseException as exc:  # unexpected: keep a value, not a crash
            rec["error"] = f"{type(exc).__name__}: {exc}"
        finally:
            if self.conn is not None:
                close_rec["conn_close"] = await close_quiet(self.conn.close())
            if self.observer is not None:
                close_rec["observer_close"] = await close_quiet(self.observer.close())
            if self.proxy is not None:
                close_rec["proxy_close"] = await close_quiet(self.proxy.close())
            rec["close"] = close_rec
            rec["seconds"] = round(time.monotonic() - started, 3)
        return rec


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    add_db_args(parser)
    parser.add_argument("--cut", type=int, required=True,
                       help="yield index to cancel at (1..4 realised in round 3b)")
    parser.add_argument("--strategy", required=True, choices=STRATEGIES)
    parser.set_defaults(reps=3, mode="direct")
    args = parser.parse_args()

    print(
        f"CASE mysqlconnector_recovery strategy={args.strategy} cut={args.cut} "
        f"connector={mysql.connector.__version__} "
        f"file={mysql.connector.__file__} python="
        f"{__import__('sys').version.split()[0]} port={args.port} db={args.dbname}",
        flush=True,
    )
    for rep in range(1, args.reps + 1):
        sc = RecoveryScenario(args, args.cut, args.strategy, rep)
        rec = await sc.run()
        print("RECOVERY " + json.dumps(rec, default=str), flush=True)
    print(f"DONE strategy={args.strategy} cut={args.cut} reps={args.reps}", flush=True)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
