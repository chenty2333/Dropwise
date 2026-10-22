"""Probe the handle a pool returns after an interrupted transaction entry.

Direct reuse and pool return/reborrow are different handoffs.  In the direct
case the interrupted connection object is the one the next caller uses.  Here
the caller exits the pool block after the cancellation, the pool decides
whether to reset, discard or reuse the connection, and only the *next*
checkout is probed.  That next handle is compared with a fresh direct
reference connection.

The pool is the mature implementation under test: psycopg_pool 3.3.3 or
asyncpg 0.32.0.  No pool source is modified.  The target is the same official
transaction context manager as in the direct experiments.

One target table and a two-table probe schema are created by an independent
asyncpg observer.  The pool connection is configured with autocommit=True
(psycopg) or default autocommit (asyncpg), so the probe's transaction is the
only transaction it opens.
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
import psycopg_pool

from cases.common import add_db_args, bound, close_quiet, err_text
from cases.txphase_case import _await_phased, install_psycopg_enter_fix

from .probe import ProbePair, compare
from .run_pg import package_info

BOUND = 5.0


def observer_dsn(args) -> str:
    return (
        f"postgresql://{args.user}:{args.password}@{args.host}:{args.port}/"
        f"{args.dbname}"
    )


class PoolProbeScenario:
    def __init__(self, args, tag: str) -> None:
        self.args = args
        self.driver = args.driver
        self.tag = tag
        self.table = f"dw_pool_probe_{os.getpid()}_{abs(hash(tag)) % 10**8}"
        self.app = f"dw-pool-probe-{os.getpid()}-{abs(hash(tag)) % 10**6}"
        self.pool = None
        self.observer: asyncpg.Connection | None = None
        self.probe_pair: ProbePair | None = None
        self.conn = None
        self.old_pid = None
        self.holder: dict = {"phase": "setup"}

    def _conninfo(self, port: int) -> str:
        a = self.args
        return (
            f"host={a.host} port={port} user={a.user} password={a.password} "
            f"dbname={a.dbname} application_name={self.app}"
        )

    def _asyncpg_dsn(self, port: int) -> str:
        a = self.args
        return (
            f"postgresql://{a.user}:{a.password}@{a.host}:{port}/{a.dbname}"
            f"?application_name={self.app}"
        )

    async def setup(self) -> None:
        self.observer = await asyncpg.connect(observer_dsn(self.args), timeout=5)
        await self.observer.execute(
            f"CREATE TABLE {self.table} (k text PRIMARY KEY, v integer NOT NULL)"
        )
        self.probe_pair = ProbePair(observer_dsn(self.args), tag=self.tag)
        await self.probe_pair.open()
        if self.driver == "psycopg":
            self.pool = psycopg_pool.AsyncConnectionPool(
                self._conninfo(self.args.port),
                min_size=1, max_size=1, open=False,
                configure=lambda conn: conn.set_autocommit(True),
                timeout=BOUND, name=self.app,
            )
            await self.pool.open(wait=True, timeout=BOUND)
        else:
            self.pool = await asyncpg.create_pool(
                self._asyncpg_dsn(self.args.port),
                min_size=1, max_size=1, timeout=BOUND,
            )
        # Preflight: one normal checkout, write, commit and independent read.
        async with self._checkout() as conn:
            await self._write(conn, "preflight", 1)
            rows = await self._rows()
            if rows != [["preflight", 1]]:
                raise RuntimeError(f"pool preflight not visible: {rows!r}")
            await self._exec(conn, f"DELETE FROM {self.table}")

    def _checkout(self):
        if self.driver == "psycopg":
            return self.pool.connection()
        return self.pool.acquire()

    async def _exec(self, conn, sql: str, params=()):
        if self.driver == "psycopg":
            cur = await conn.execute(sql, params)
            return cur.statusmessage
        return await conn.execute(sql, *params)

    async def _write(self, conn, key: str, value: int) -> str:
        if self.driver == "psycopg":
            return await self._exec(
                conn, f"INSERT INTO {self.table} (k, v) VALUES (%s, %s) "
                      "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
                (key, value),
            )
        return await self._exec(
            conn, f"INSERT INTO {self.table} (k, v) VALUES ($1, $2) "
                  "ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
            (key, value),
        )

    async def _tx_target(self, conn):
        self.holder["phase"] = "enter"
        async with conn.transaction():
            self.holder["phase"] = "body"
            await self._write(conn, "target", 1)
            self.holder["phase"] = "body_wait"
            await asyncio.sleep(0.2)

    async def _fetchval(self, conn, sql: str, param):
        if self.driver == "psycopg":
            cur = await conn.execute(sql, (param,))
            row = await cur.fetchone()
            return None if row is None else row[0]
        return await conn.fetchval(sql, param)

    async def _rows(self):
        assert self.observer is not None
        recs = await self.observer.fetch(
            f"SELECT k, v FROM {self.table} ORDER BY k"
        )
        return [[r["k"], r["v"]] for r in recs]

    def _stats(self) -> dict:
        if self.driver == "psycopg":
            s = self.pool.get_stats()
            return {k: s.get(k) for k in (
                "pool_min", "pool_max", "pool_size", "pool_available",
                "requests_waiting", "requests_queued", "connections_lost",
                "connections_num",
            )}
        return {
            "size": self.pool.get_size(), "idle": self.pool.get_idle_size(),
            "min": self.pool.get_min_size(), "max": self.pool.get_max_size(),
        }

    async def _server(self) -> dict:
        assert self.observer is not None
        try:
            rows = await self.observer.fetch(
                "SELECT pid, state, left(query, 80) AS query "
                "FROM pg_stat_activity WHERE application_name = $1 "
                "AND pid <> pg_backend_pid() ORDER BY pid",
                self.app,
            )
            return {"pids": [r["pid"] for r in rows],
                    "sessions": [[r["pid"], r["state"], r["query"]]
                                 for r in rows]}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def _pid(self, conn) -> int | None:
        try:
            if self.driver == "psycopg":
                return conn.info.backend_pid
            return conn.get_server_pid()
        except BaseException:
            return None

    async def _probe_on(self, handle: str, conn, *, probe_timeout: float = 2.0):
        assert self.probe_pair is not None
        return await self.probe_pair.run(
            handle, conn, self.driver, marker=f"{handle}-{self.tag}",
            probe_timeout=probe_timeout,
        )

    async def trial(self, k: int, variant: str) -> dict:
        rec: dict = {"driver": self.driver, "k": k, "variant": variant,
                     "repair": getattr(self.args, "repair", "none"),
                     "package": package_info(self.driver),
                     "tag": self.tag, "table": self.table}
        started = time.monotonic()
        await self.setup()
        rec["setup"] = {"pid": await self._pid_any(), "stats": self._stats()}
        rec["pre_server"] = await self._server()
        tr = None
        meta = None
        try:
            async with self._checkout() as conn:
                self.conn = conn
                self.old_pid = await self._pid(conn)
                rec["old_pid"] = self.old_pid
                tr, meta = await _await_phased(
                    self._tx_target(conn), cut_after=k, timeout=12,
                    holder=self.holder, double=False,
                )
                rec["track_phase"] = meta["cut_phase"]
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            rec["pool_block_error"] = err_text(exc)
        finally:
            self.conn = None
        rec["target"] = {
            "outcome": None if tr is None else tr.outcome,
            "realized": None if tr is None else tr.realized,
            "yields": None if tr is None else tr.yields,
            "error": None if tr is None else tr.error,
            "seconds": None if tr is None else round(tr.seconds, 4),
        }
        rec["cut_phase"] = None if meta is None else meta["cut_phase"]
        rec["post_pool"] = {"stats": self._stats(),
                            "server": await self._server()}
        await asyncio.sleep(0.08)

        # Next checkout: the pool has decided what to do with the old handle.
        try:
            async with self._checkout() as next_conn:
                rec["next_pid"] = await self._pid(next_conn)
                rec["next_same_pid"] = rec["next_pid"] == self.old_pid
                old_probe = await self._probe_on("old", next_conn)
                rec["probe_old"] = old_probe.as_dict()
                rec["survivor_write"] = await self._write(next_conn, "survivor", 7)
                rec["survivor_own"] = await self._fetchval(
                    next_conn,
                    f"SELECT v FROM {self.table} WHERE k = %s"
                    if self.driver == "psycopg"
                    else f"SELECT v FROM {self.table} WHERE k = $1",
                    "survivor",
                )
                rec["probe_visible_before_return"] = (
                    await self.probe_pair.persisted_after_close(
                        "old", f"old-{self.tag}"
                    )
                )
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            rec["next_checkout_error"] = err_text(exc)
        rec["after_next"] = {"stats": self._stats(),
                             "server": await self._server(),
                             "rows": await self._rows()}

        # Fresh direct reference handle.
        try:
            ref = await self._reference_connect()
            try:
                ref_probe = await self._probe_on("ref", ref)
            finally:
                await ref.close()
            rec["probe_ref"] = ref_probe.as_dict()
            if "probe_old" in rec:
                rec["probe_compare"] = compare(
                    _to_result(rec["probe_old"]), ref_probe
                )
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            rec["reference_error"] = err_text(exc)
        rec["final_rows"] = await self._rows()
        rec["final"] = {"stats": self._stats(), "server": await self._server()}
        rec["seconds"] = round(time.monotonic() - started, 3)
        return rec

    async def _reference_connect(self):
        if self.driver == "psycopg":
            import psycopg
            return await psycopg.AsyncConnection.connect(
                self._conninfo(self.args.port), autocommit=True,
                connect_timeout=5,
            )
        return await asyncpg.connect(self._asyncpg_dsn(self.args.port), timeout=5)

    async def _pid_any(self):
        try:
            async with self._checkout() as conn:
                return await self._pid(conn)
        except BaseException:
            return None

    async def close(self) -> None:
        if self.pool is not None:
            if self.driver == "psycopg":
                await close_quiet(self.pool.close(timeout=2.0))
            else:
                await close_quiet(self.pool.close())
            self.pool = None
        if self.probe_pair is not None:
            await close_quiet(self.probe_pair.close())
            self.probe_pair = None
        if self.observer is not None:
            await close_quiet(self.observer.execute(
                f"DROP TABLE IF EXISTS {self.table}"
            ))
            await close_quiet(self.observer.close())
            self.observer = None


def _to_result(d: dict):
    from .probe import ProbeResult
    return ProbeResult(
        handle=d["handle"], status=d["status"], anomaly=d["anomaly"],
        applicable=d["applicable"], checks=d.get("checks", {}),
        seconds=d.get("seconds", 0.0), error=d.get("error"),
    )


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--driver", choices=["psycopg", "asyncpg"], required=True)
    ap.add_argument("--cuts", default="1", help="comma-separated cut indices")
    ap.add_argument("--variant", default="original", choices=["original", "fix"])
    ap.add_argument("--repair", choices=["none", "experimental", "actual"],
                    default=None)
    ap.set_defaults(reps=1)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    if args.repair is None:
        args.repair = "experimental" if args.variant == "fix" else "none"
    if args.repair == "experimental" and args.driver == "psycopg":
        install_psycopg_enter_fix()
    if args.repair == "actual":
        info = package_info(args.driver)
        marker = ("has_recover_entry_failure" if args.driver == "psycopg"
                  else "start_has_top_xact_clear")
        if not info.get(marker):
            raise SystemExit(
                f"--repair actual requested but loaded package lacks {marker}: "
                f"{info}"
            )

    cuts = [int(x) for x in args.cuts.split(",") if x.strip()]
    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for k in cuts:
                sc = PoolProbeScenario(
                    args, tag=f"{args.driver}-{args.repair}-{rep}-{k}")
                try:
                    rec = await asyncio.wait_for(sc.trial(k, args.variant), 40)
                    rec["rep"] = rep
                except asyncio.CancelledError:
                    raise
                except BaseException as exc:
                    failures += 1
                    rec = {"driver": args.driver, "k": k, "rep": rep,
                           "trial_error": err_text(exc),
                           "trace": __import__("traceback").format_exc()[-1200:]}
                try:
                    rec = _san(rec, sc.table)
                except BaseException:
                    pass
                print("POOLPROBE " + json.dumps(rec, default=str, sort_keys=True),
                      file=out, flush=True)
                c = rec.get("probe_compare") or {}
                print(
                    f"POOLPROBE-SUMMARY driver={args.driver} "
                    f"repair={args.repair} k={k} rep={rep} "
                    f"phase={rec.get('cut_phase')} "
                    f"next_same_pid={rec.get('next_same_pid')} "
                    f"old={((rec.get('probe_old') or {}).get('status'))} "
                    f"ref={((rec.get('probe_ref') or {}).get('status'))} "
                    f"compare={c.get('label')} "
                    f"rows={rec.get('final_rows')} "
                    f"seconds={rec.get('seconds')}",
                    flush=True,
                )
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0


def _san(value, table: str):
    if isinstance(value, str):
        return value.replace(table, "<table>")
    if isinstance(value, dict):
        return {str(k): _san(v, table) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_san(v, table) for v in value]
    return value


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
