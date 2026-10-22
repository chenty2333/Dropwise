"""A bounded successor probe for reused PostgreSQL handles.

The probe is deliberately not a repair action.  It runs on a handle *after* an
interrupted operation and asks whether that handle still behaves like a newly
created reference handle for a small, explicit contract:

1. a fresh nonce echo returns the nonce and the agreed column metadata;
2. a normal transaction writes a unique marker, exits, and an independent
   connection sees the marker;
3. closing the probe's handle does not change that observation.

The transaction/write check is the part that a value-only echo cannot replace:
a poisoned handle can echo correctly and still turn the probe's COMMIT into a
SAVEPOINT release inside an abandoned transaction.  In that case the probe
reports an invisible write even though the driver returned success.

This is a probe, not an oracle.  It knows nothing about the interrupted
operation's own effect.  Its applicability is a handoff contract: no live owner
and no open transaction.  A legitimate outer transaction is recorded as
``open_transaction_allowed`` only when the caller declares that state; it is
not silently treated as clean.  Session state is recorded but is not compared,
because retaining an application_name, a GUC, a temporary table or a prepared
statement is legal.

The independent observer is a separate asyncpg connection.  It creates one
schema with an ``old`` and a ``ref`` table per probe pair, so the two handles
never write to the same namespace.  The tested handle only executes DML in its
own table; DDL is done by the observer, outside both handles.
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses
import enum
import itertools
import os
import re
import time

import asyncpg

_PROBE_SEQ = itertools.count(1)
_SAFE = re.compile(r"[^a-z0-9_]+")


class ProbeStatus(str, enum.Enum):
    CLEAN = "clean"
    OPEN_TRANSACTION = "open_transaction"
    OPEN_TRANSACTION_ALLOWED = "open_transaction_allowed"
    CLOSED = "closed"
    ECHO_MISMATCH = "echo_mismatch"
    METADATA_MISMATCH = "metadata_mismatch"
    PROTOCOL_ERROR = "protocol_error"
    TRANSACTION_API_ERROR = "transaction_api_error"
    INVISIBLE_WRITE = "invisible_write"
    WRITE_ONLY_VISIBLE_AFTER_SETTLE = "write_only_visible_after_settle"
    BLOCKED_BY_SESSION = "blocked_by_session"
    PROBE_TIMEOUT = "probe_timeout"
    HARNESS_ERROR = "harness_error"


# Statuses in this set are differences of the handle from the reference under
# the stated contract.  BLOCKED/PROBE_TIMEOUT are unprobeable, not anomalies.
ANOMALY = {
    ProbeStatus.OPEN_TRANSACTION,
    ProbeStatus.CLOSED,
    ProbeStatus.ECHO_MISMATCH,
    ProbeStatus.METADATA_MISMATCH,
    ProbeStatus.PROTOCOL_ERROR,
    ProbeStatus.TRANSACTION_API_ERROR,
    ProbeStatus.INVISIBLE_WRITE,
    ProbeStatus.WRITE_ONLY_VISIBLE_AFTER_SETTLE,
}


@dataclasses.dataclass
class ProbeResult:
    handle: str
    status: str
    anomaly: bool
    applicable: bool
    checks: dict
    seconds: float = 0.0
    error: str | None = None

    def as_dict(self) -> dict:
        return {
            "handle": self.handle,
            "status": self.status,
            "anomaly": self.anomaly,
            "applicable": self.applicable,
            "error": self.error,
            "seconds": round(self.seconds, 4),
            "checks": self.checks,
        }


def _status(value: ProbeStatus) -> str:
    return value.value


def _err(exc: BaseException) -> str:
    return f"{type(exc).__name__}: {exc}"


def _is_config_error(exc: BaseException) -> bool:
    """A legal session configuration may prevent the write probe.

    Read-only sessions, statement timeouts and missing privileges are not
    handle contamination.  The function is intentionally textual: the probe is
    used with two drivers and a disposable server, and the classification is
    recorded so a reader can reject it.
    """
    text = f"{type(exc).__name__}: {exc}".lower()
    markers = (
        "read-only transaction",
        "cannot execute insert in a read-only transaction",
        "statement timeout",
        "canceling statement due to statement timeout",
        "permission denied",
        "insufficient privilege",
    )
    return any(m in text for m in markers)


class ProbePair:
    """One observer, one schema, one old table and one reference table."""

    def __init__(self, observer_dsn: str, tag: str = "probe") -> None:
        self.observer_dsn = observer_dsn
        self.tag = _SAFE.sub("_", tag.lower())
        self.observer: asyncpg.Connection | None = None
        self.schema = f"dw_succ_{self.tag}_{os.getpid()}_{next(_PROBE_SEQ)}"
        self.tables = {"old": f"{self.schema}.old", "ref": f"{self.schema}.ref"}
        self.created = False

    async def open(self) -> None:
        self.observer = await asyncpg.connect(self.observer_dsn, timeout=5)
        await self.observer.execute(f"CREATE SCHEMA {self.schema}")
        for name in ("old", "ref"):
            await self.observer.execute(
                f"CREATE TABLE {self.schema}.{name} "
                "(marker text PRIMARY KEY, value integer NOT NULL)"
            )
        self.created = True

    async def close(self) -> None:
        if self.observer is None:
            return
        if self.created:
            with contextlib.suppress(BaseException):
                await self.observer.execute(f"DROP SCHEMA {self.schema} CASCADE")
        with contextlib.suppress(BaseException):
            await self.observer.close()
        self.observer = None

    async def _visible(self, table: str, marker: str, timeout: float) -> bool:
        assert self.observer is not None
        deadline = time.monotonic() + timeout
        while True:
            value = await self.observer.fetchval(
                f"SELECT value FROM {table} WHERE marker = $1", marker
            )
            if value is not None:
                return True
            if time.monotonic() >= deadline:
                return False
            await asyncio.sleep(0.01)

    async def run(
        self,
        handle: str,
        conn,
        driver: str,
        *,
        marker: str,
        allow_open_transaction: bool = False,
        probe_timeout: float = 2.0,
        visibility_timeout: float = 1.0,
        write: bool = True,
    ) -> ProbeResult:
        """Run the bounded probe on one handle.

        The handle is left open.  The caller may close it and call
        :meth:`persisted_after_close` to check that a successful-looking write
        was a real commit rather than a savepoint release.
        """
        started = time.monotonic()
        checks: dict = {
            "driver": driver,
            "marker": marker,
            "allow_open_transaction": allow_open_transaction,
            "probe_timeout": probe_timeout,
            "visibility_timeout": visibility_timeout,
        }
        table = self.tables[handle]

        def result(status: ProbeStatus, *, applicable: bool | None = None,
                   error: str | None = None) -> ProbeResult:
            anomaly = status in ANOMALY
            if applicable is None:
                applicable = status not in (
                    ProbeStatus.PROBE_TIMEOUT, ProbeStatus.BLOCKED_BY_SESSION,
                    ProbeStatus.OPEN_TRANSACTION_ALLOWED,
                )
            checks["anomaly"] = anomaly
            checks["applicable"] = applicable
            return ProbeResult(
                handle=handle, status=_status(status), anomaly=anomaly,
                applicable=applicable, checks=checks,
                seconds=time.monotonic() - started, error=error,
            )

        try:
            state = await self._state(conn, driver)
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            checks["state_error"] = _err(exc)
            return result(ProbeStatus.HARNESS_ERROR, applicable=False,
                          error=_err(exc))
        checks["state_before"] = state

        if state.get("closed"):
            return result(ProbeStatus.CLOSED, error="handle is closed",
                          applicable=True)

        # 1. Fresh nonce echo + metadata, with a bounded wait. -----------------
        nonce = f"{marker}-{time.time_ns()}"
        try:
            echo = await asyncio.wait_for(
                self._echo(conn, driver, nonce), probe_timeout
            )
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            checks["echo_error"] = f"timeout after {probe_timeout:.3f}s"
            return result(ProbeStatus.PROBE_TIMEOUT,
                          error=checks["echo_error"])
        except BaseException as exc:
            checks["echo_error"] = _err(exc)
            return result(ProbeStatus.PROTOCOL_ERROR, error=_err(exc))
        checks["echo"] = echo
        if echo["value"] != nonce or echo["answer"] != 42:
            checks["expected"] = {"value": nonce, "answer": 42}
            return result(ProbeStatus.ECHO_MISMATCH,
                          error="nonce echo did not return its own value")
        if echo["columns"] != [{"name": "nonce", "type_code": 25},
                               {"name": "answer", "type_code": 23}]:
            checks["expected_columns"] = [
                {"name": "nonce", "type_code": 25},
                {"name": "answer", "type_code": 23},
            ]
            return result(ProbeStatus.METADATA_MISMATCH,
                          error="echo column metadata changed")

        # 2. Session state is diagnostic only; retained state is legitimate. ---
        try:
            checks["session"] = await asyncio.wait_for(
                self._session(conn, driver), probe_timeout
            )
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            checks["session_error"] = _err(exc)
            # The echo already proved the stream is usable for reads.  A
            # session query failing is not a handle-contamination verdict.
            checks["session"] = None

        # 3. Boundary: an open transaction is not a clean handoff unless the
        # caller explicitly declares a live outer transaction. -----------------
        try:
            state = await self._state(conn, driver)
            checks["state_after_echo"] = state
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            checks["state_after_echo_error"] = _err(exc)
            return result(ProbeStatus.HARNESS_ERROR, applicable=False,
                          error=_err(exc))
        if state.get("in_transaction"):
            if allow_open_transaction:
                return result(ProbeStatus.OPEN_TRANSACTION_ALLOWED)
            return result(
                ProbeStatus.OPEN_TRANSACTION,
                error="handle is in a transaction at the declared handoff",
            )

        if not write:
            return result(ProbeStatus.CLEAN)

        # 4. Normal transaction write + commit. --------------------------------
        write_started = time.monotonic()
        try:
            write_info = await asyncio.wait_for(
                self._transaction_write(conn, driver, table, marker, 7),
                probe_timeout,
            )
            checks["write"] = write_info
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            checks["write_error"] = f"timeout after {probe_timeout:.3f}s"
            checks["write_seconds"] = round(time.monotonic() - write_started, 4)
            return result(ProbeStatus.PROBE_TIMEOUT,
                          error=checks["write_error"])
        except BaseException as exc:
            checks["write_error"] = _err(exc)
            checks["write_seconds"] = round(time.monotonic() - write_started, 4)
            if _is_config_error(exc):
                return result(ProbeStatus.BLOCKED_BY_SESSION,
                              applicable=False, error=_err(exc))
            return result(ProbeStatus.TRANSACTION_API_ERROR,
                          error=_err(exc))
        checks["write_seconds"] = round(time.monotonic() - write_started, 4)

        # 5. Independent visibility, with a bounded settle window. -------------
        vis_started = time.monotonic()
        try:
            visible = await asyncio.wait_for(
                self._visible(table, marker, visibility_timeout),
                visibility_timeout + probe_timeout,
            )
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            checks["visibility_error"] = _err(exc)
            return result(ProbeStatus.HARNESS_ERROR, applicable=False,
                          error=_err(exc))
        checks["visible"] = visible
        checks["visibility_seconds"] = round(time.monotonic() - vis_started, 4)
        if not visible:
            return result(
                ProbeStatus.INVISIBLE_WRITE,
                error="transactional write was not visible from the observer",
            )
        if checks["visibility_seconds"] > 0.05:
            return result(ProbeStatus.WRITE_ONLY_VISIBLE_AFTER_SETTLE)
        return result(ProbeStatus.CLEAN)

    async def persisted_after_close(
        self, handle: str, marker: str, *, timeout: float = 0.5
    ) -> dict:
        """After the caller closes the handle, is the marker still visible?"""
        table = self.tables[handle]
        try:
            visible = await self._visible(table, marker, timeout)
            return {"visible": visible, "table": table}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"visible": None, "error": _err(exc), "table": table}

    # ---- driver helpers -------------------------------------------------
    async def _state(self, conn, driver: str) -> dict:
        if driver == "psycopg":
            status = conn.info.transaction_status
            name = getattr(status, "name", str(status))
            return {
                "driver": "psycopg",
                "closed": bool(conn.closed),
                "transaction_status": name,
                "in_transaction": name in ("INTRANS", "INERROR", "ACTIVE"),
            }
        if driver == "asyncpg":
            closed = bool(conn.is_closed())
            if closed:
                return {
                    "driver": "asyncpg",
                    "closed": True,
                    "in_transaction": None,
                    "top_xact": type(getattr(conn, "_top_xact", None)).__name__,
                }
            in_tx = bool(conn.is_in_transaction())
            return {
                "driver": "asyncpg",
                "closed": False,
                "in_transaction": in_tx,
                "top_xact": type(getattr(conn, "_top_xact", None)).__name__,
            }
        raise ValueError(f"unknown driver {driver!r}")

    async def _echo(self, conn, driver: str, nonce: str) -> dict:
        if driver == "psycopg":
            cur = await conn.execute(
                "SELECT %s::text AS nonce, %s::int4 AS answer", (nonce, 42)
            )
            row = await cur.fetchone()
            columns = [
                {"name": d.name, "type_code": d.type_code}
                for d in (cur.description or [])
            ]
            return {
                "value": None if row is None else row[0],
                "answer": None if row is None else row[1],
                "columns": columns,
            }
        if driver == "asyncpg":
            stmt = await conn.prepare(
                "SELECT $1::text AS nonce, $2::int4 AS answer"
            )
            columns = [
                {"name": attr.name, "type_code": attr.type.oid}
                for attr in stmt.get_attributes()
            ]
            rec = await stmt.fetchrow(nonce, 42)
            return {
                "value": None if rec is None else rec["nonce"],
                "answer": None if rec is None else rec["answer"],
                "columns": columns,
            }
        raise ValueError(f"unknown driver {driver!r}")

    async def _session(self, conn, driver: str) -> dict:
        sql = (
            "SELECT current_setting('application_name') AS app, "
            "       current_setting('search_path') AS search_path, "
            "       pg_backend_pid() AS pid, "
            "       current_setting('transaction_isolation') AS isolation, "
            "       current_setting('default_transaction_read_only') AS read_only"
        )
        if driver == "psycopg":
            cur = await conn.execute(sql)
            row = await cur.fetchone()
            return {
                "app": row[0], "search_path": row[1], "pid": row[2],
                "isolation": row[3], "read_only": row[4],
            }
        rec = await conn.fetchrow(sql)
        return {
            "app": rec["app"], "search_path": rec["search_path"],
            "pid": rec["pid"], "isolation": rec["isolation"],
            "read_only": rec["read_only"],
        }

    async def _transaction_write(self, conn, driver: str, table: str,
                                 marker: str, value: int) -> dict:
        if driver == "psycopg":
            async with conn.transaction():
                cur = await conn.execute(
                    f"INSERT INTO {table} (marker, value) VALUES (%s, %s)",
                    (marker, value),
                )
                return {"statusmessage": cur.statusmessage}
        if driver == "asyncpg":
            async with conn.transaction():
                status = await conn.execute(
                    f"INSERT INTO {table} (marker, value) VALUES ($1, $2)",
                    marker, value,
                )
                return {"statusmessage": status}
        raise ValueError(f"unknown driver {driver!r}")


def compare(old: ProbeResult, ref: ProbeResult) -> dict:
    """Compare the interrupted-reuse handle with a fresh reference handle.

    A reference anomaly is an environment/harness problem, not evidence about
    the old handle.  The returned label is intentionally coarser than the two
    statuses; the statuses are kept in the record.
    """
    if ref.anomaly:
        label = "reference_anomaly"
    elif not ref.applicable:
        label = "reference_not_applicable"
    elif old.status == ProbeStatus.CLOSED.value:
        label = "old_closed"
    elif old.anomaly:
        label = "old_anomaly"
    elif old.status == ProbeStatus.CLEAN.value and ref.status == ProbeStatus.CLEAN.value:
        label = "consistent_clean"
    elif old.status == ProbeStatus.OPEN_TRANSACTION_ALLOWED.value:
        label = "legitimate_open_transaction"
    elif not old.applicable:
        label = "old_not_applicable"
    else:
        label = "different_non_anomalous"
    return {
        "label": label,
        "old_status": old.status,
        "ref_status": ref.status,
        "old_anomaly": old.anomaly,
        "ref_anomaly": ref.anomaly,
        "old_applicable": old.applicable,
        "ref_applicable": ref.applicable,
    }
