"""Shared helpers for the pool-ACQUIRE cancellation cases.

Design rules that matter for the evidence:

* An acquire is bounded by the pool's own ``timeout=2.0``; every diagnostic
  await is wrapped in ``bound(..., 2.0)`` (or an ``asyncio.timeout(2)`` block).
* ``CancelledError`` is recorded only at the exact point it is caught and is
  always re-raised.  No helper converts it into a value.
* Diagnostics read the pool through its public stats plus *read-only* private
  containers (``_waiting`` / ``_queue``).  They never ask a connection object
  for anything after it has been returned to the pool; ``safe_*_pid`` exists
  only to keep a late diagnostic from being misreported as a target failure
  (the round-3 released-connection artifact).
* The observer is an independent direct connection, with its own
  ``application_name``, never obtained from the tested pool.
"""

from __future__ import annotations

import asyncio
import itertools
import os
import time
from pathlib import Path

from cases.common import BOUND, bound, close_quiet, err_text, sanitize  # noqa: F401

_app_seq = itertools.count(1)


def unique_app(tag: str) -> str:
    """A short, unique application_name (PostgreSQL truncates around 63 B)."""
    return f"dwpa-{tag}-{os.getpid()}-{next(_app_seq)}"


def pg_conninfo(host, port, user, password, dbname, app: str) -> str:
    return (
        f"host={host} port={port} user={user} password={password} "
        f"dbname={dbname} connect_timeout=5 application_name={app}"
    )


def apg_dsn(host, port, user, password, dbname, app: str) -> str:
    return (
        f"postgresql://{user}:{password}@{host}:{port}/{dbname}"
        f"?application_name={app}"
    )


async def wait_until(predicate, timeout: float = 1.0, step: float = 0.005):
    """Poll a *sync* predicate without touching the tested objects.

    Returns ``(ok, polls, elapsed)``.  Used only by the deterministic
    handoff-window race, where the cut has to happen before the awakened
    waiter gets its next event-loop step.
    """
    started = time.monotonic()
    polls = 0
    while True:
        polls += 1
        try:
            if predicate():
                return True, polls, time.monotonic() - started
        except BaseException:
            return False, polls, time.monotonic() - started
        if time.monotonic() - started >= timeout:
            return False, polls, time.monotonic() - started
        await asyncio.sleep(step)


# ---------------------------------------------------------------------
# psycopg_pool diagnostics
# ---------------------------------------------------------------------
PG_STAT_KEYS = (
    "pool_size",
    "pool_available",
    "requests_waiting",
    "requests_num",
    "requests_queued",
    "requests_errors",
    "connections_num",
    "connections_lost",
    "returns_bad",
)


def pg_stats(pool) -> dict:
    """Public stats snapshot; a diagnostic failure is returned, never raised."""
    if pool is None:
        return {"error": "no pool"}
    try:
        raw = dict(pool.get_stats())
        return {key: raw.get(key) for key in PG_STAT_KEYS}
    except BaseException as exc:
        return {"error": err_text(exc)}


def pg_private(pool) -> dict:
    """Read-only view of the waiter/free-list state (not an atomic snapshot)."""
    if pool is None:
        return {"error": "no pool"}
    try:
        waiting = []
        for pos in list(pool._waiting):
            waiting.append(
                {
                    "has_conn": pos.conn is not None,
                    "error": type(pos.error).__name__ if pos.error else None,
                }
            )
        return {
            "nconns": pool._nconns,
            "waiting_len": len(pool._waiting),
            "available_len": len(pool._pool),
            "waiting": waiting,
        }
    except BaseException as exc:
        return {"error": err_text(exc)}


def safe_pg_pid(conn):
    """Only call while the connection is checked out by this task."""
    try:
        return conn.info.backend_pid
    except BaseException as exc:  # e.g. already put back / closed
        return f"unavailable: {type(exc).__name__}: {exc}"


def safe_pg_status(conn):
    try:
        return conn.info.transaction_status.name
    except BaseException as exc:
        return f"unavailable: {type(exc).__name__}: {exc}"


class PgObserver:
    """Independent direct psycopg connection; never routed through the pool."""

    def __init__(self, conninfo: str) -> None:
        self.conninfo = conninfo
        self.conn = None

    async def connect(self) -> None:
        import psycopg

        try:
            self.conn = await asyncio.wait_for(
                psycopg.AsyncConnection.connect(self.conninfo, autocommit=True),
                BOUND,
            )
            await asyncio.wait_for(self.conn.execute("SELECT 1"), BOUND)
        except BaseException:
            await self.close()
            raise

    async def sample(self, app: str) -> dict:
        """Server-side backend count/pid set for the tested application_name."""
        if self.conn is None:
            return {"error": "observer not connected"}
        try:
            cur = await asyncio.wait_for(
                self.conn.execute(
                    "SELECT count(*), "
                    "coalesce(array_agg(pid ORDER BY pid), ARRAY[]::int[]) "
                    "FROM pg_stat_activity "
                    "WHERE application_name = %s AND pid <> pg_backend_pid()",
                    (app,),
                ),
                BOUND,
            )
            row = await asyncio.wait_for(cur.fetchone(), BOUND)
            return {"count": row[0], "pids": list(row[1] or [])}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def close(self) -> None:
        if self.conn is not None:
            conn, self.conn = self.conn, None
            await close_quiet(conn.close())


# ---------------------------------------------------------------------
# asyncpg diagnostics
# ---------------------------------------------------------------------
def apg_stats(pool) -> dict:
    if pool is None:
        return {"error": "no pool"}
    try:
        holders = []
        for holder in list(pool._holders):
            try:
                holders.append(
                    {
                        "connected": holder.is_connected(),
                        "idle": holder.is_idle(),
                        "in_use": holder._in_use is not None,
                    }
                )
            except BaseException as exc:
                holders.append({"error": err_text(exc)})
        return {
            "size": pool.get_size(),
            "idle": pool.get_idle_size(),
            "min": pool.get_min_size(),
            "max": pool.get_max_size(),
            "qsize": pool._queue.qsize(),
            "holders": holders,
        }
    except BaseException as exc:
        return {"error": err_text(exc)}


def safe_apg_pid(conn):
    """Only call while the proxy is checked out by this task."""
    try:
        return conn.get_server_pid()
    except BaseException as exc:  # InterfaceError after release (round-3 artifact)
        return f"unavailable: {type(exc).__name__}: {exc}"


class ApgObserver:
    """Independent direct asyncpg connection; never routed through the pool."""

    def __init__(self, dsn: str) -> None:
        self.dsn = dsn
        self.conn = None

    async def connect(self) -> None:
        import asyncpg

        try:
            self.conn = await asyncio.wait_for(
                asyncpg.connect(self.dsn, timeout=BOUND), BOUND
            )
            await asyncio.wait_for(self.conn.execute("SELECT 1"), BOUND)
        except BaseException:
            await self.close()
            raise

    async def sample(self, app: str) -> dict:
        if self.conn is None:
            return {"error": "observer not connected"}
        try:
            row = await asyncio.wait_for(
                self.conn.fetchrow(
                    "SELECT count(*) AS n, "
                    "coalesce(array_agg(pid ORDER BY pid), ARRAY[]::int[]) AS pids "
                    "FROM pg_stat_activity "
                    "WHERE application_name = $1 AND pid <> pg_backend_pid()",
                    app,
                ),
                BOUND,
            )
            return {"count": row["n"], "pids": list(row["pids"] or [])}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def close(self) -> None:
        if self.conn is not None:
            conn, self.conn = self.conn, None
            await close_quiet(conn.close())


# ---------------------------------------------------------------------
# Small record helpers
# ---------------------------------------------------------------------
def target_record(tr) -> dict:
    return {
        "outcome": tr.outcome,
        "yields": tr.yields,
        "cut_requested": tr.cut_requested,
        "realized": tr.realized,
        "error": tr.error,
        "seconds": round(tr.seconds, 6),
    }


def missing(rec: dict, *path, default=None):
    cur = rec
    for key in path:
        if not isinstance(cur, dict) or key not in cur:
            return default
        cur = cur[key]
    return cur


def clean_diag(rec: dict) -> dict:
    """Strip run-specific table/app names and loopback ports for the log."""
    return sanitize(rec)


def here() -> Path:
    return Path(__file__).resolve()
