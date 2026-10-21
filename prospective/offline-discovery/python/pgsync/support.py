"""Support for the offline-discovery sync-interruption experiments.

The object under test is psycopg 3.3.6's *synchronous* transaction context
manager.  Three observation layers are used together:

* client: public ``conn.info.transaction_status`` plus the private
  ``conn._num_transactions`` counter, which is what the library's own
  ``commit()``/``rollback()`` guards and the transaction stack check read;
* server: an independent connection samples ``pg_stat_activity`` for the tested
  backend, and reads committed rows;
* wire: a loopback proxy forwards PostgreSQL messages with a configurable
  per-direction delay and logs parsed frames (Q/P/C/Z/E/...).

A "real signal" is a POSIX interval timer (``SIGALRM``) whose Python handler is
the same kind of handler CPython installs for ``SIGINT``: it records the frame
that was executing and raises ``KeyboardInterrupt``.  This differs from
``SIGINT`` only in the signal number; the asynchronous-exception semantics are
the interpreter's.  ``SigIntProbe`` additionally supports a real ``SIGINT`` sent
from a helper thread, used as an equivalence check for the wait window.

Nothing here modifies psycopg.  The deterministic ``TraceRaise`` injector is an
explicitly *synthetic* interruption at a chosen source line; it is used only to
construct a state whose real-signal reachability is demonstrated separately by
the timing sweep.  Reports keep the two evidence kinds apart.
"""

from __future__ import annotations

import contextlib
import json
import os
import signal
import subprocess
import sys
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

import psycopg

HERE = Path(__file__).resolve().parent
PROXY = HERE / "proxy.py"


# --------------------------------------------------------------------------
# database helpers
# --------------------------------------------------------------------------

def dsn(port: int, host: str = "127.0.0.1") -> str:
    return (f"host={host} port={port} user=postgres password=dw "
            "dbname=dw connect_timeout=5")


def connect(port: int, autocommit: bool = True):
    return psycopg.connect(dsn(port), autocommit=autocommit)


def setup_table(observer, table: str) -> None:
    observer.execute(f"DROP TABLE IF EXISTS {table}")
    observer.execute(f"CREATE TABLE {table} (k text primary key, v integer not null)")


def table_rows(observer, table: str):
    cur = observer.execute(f"SELECT k, v FROM {table} ORDER BY k")
    return [[r[0], r[1]] for r in cur.fetchall()]


def client_state(conn) -> dict:
    """Client-side facts.  _num_transactions is private but is the actual stack
    counter that public commit()/rollback() and the nest check use."""
    try:
        return {
            "tx": conn.info.transaction_status.name,
            "num_transactions": conn._num_transactions,
            "closed": conn.closed,
            "pgconn_status": str(conn.pgconn.status),
        }
    except BaseException as exc:  # diagnostics must not mask the trial
        return {"error": type(exc).__name__ + ": " + str(exc)}


def server_state(observer, pid: int) -> dict | None:
    cur = observer.execute(
        "SELECT state, (xact_start IS NOT NULL)::text, backend_xid::text, "
        "left(query, 60) FROM pg_stat_activity WHERE pid = %s", (pid,))
    row = cur.fetchone()
    if row is None:
        return None
    return {"state": row[0], "in_xact": row[1], "xid": row[2], "query": row[3]}


def server_pid(conn) -> int:
    return conn.info.backend_pid


# --------------------------------------------------------------------------
# wire proxy
# --------------------------------------------------------------------------

class FrameProxy:
    """Subprocess proxy; one extra process, no thread-per-connection."""

    def __init__(self, target_port: int, *, c2s_delay: float = 0.0,
                 s2c_delay: float = 0.0, log_path: str | Path | None = None):
        self.target_port = target_port
        self.c2s_delay = c2s_delay
        self.s2c_delay = s2c_delay
        self.log_path = str(log_path) if log_path else None
        self.proc: subprocess.Popen | None = None
        self.port: int | None = None

    def start(self) -> int:
        args = [sys.executable, str(PROXY), "--target-port", str(self.target_port),
                "--c2s-delay", str(self.c2s_delay), "--s2c-delay", str(self.s2c_delay)]
        if self.log_path:
            args += ["--log", self.log_path]
        self.proc = subprocess.Popen(args, stdout=subprocess.PIPE, text=True)
        assert self.proc.stdout is not None
        line = self.proc.stdout.readline().strip()
        if not line.startswith("PORT "):
            raise RuntimeError(f"proxy did not start: {line!r}")
        self.port = int(line.split()[1])
        return self.port

    def frames(self) -> list[dict]:
        if not self.log_path or not os.path.exists(self.log_path):
            return []
        out = []
        with open(self.log_path) as f:
            for line in f:
                with contextlib.suppress(json.JSONDecodeError):
                    out.append(json.loads(line))
        return out

    def close(self) -> None:
        if self.proc is None:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.proc = None

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc):
        self.close()


# --------------------------------------------------------------------------
# interruption injectors
# --------------------------------------------------------------------------

@dataclass
class Landing:
    """Where an asynchronous exception was delivered, from the signal handler."""
    frames: list[tuple[str, str, int]]
    fired_at: float | None = None
    armed_at: float | None = None

    @property
    def text(self) -> str:
        return " <- ".join(f"{f}:{n}:{l}" for f, n, l in self.frames)

    def contains(self, filename_suffix: str, func: str | None = None) -> bool:
        for f, n, _l in self.frames:
            if f.endswith(filename_suffix) and (func is None or n == func):
                return True
        return False


class SignalProbe:
    """Own the SIGALRM handler that raises KeyboardInterrupt.

    Only one probe can be active per process.  ``arm_timer`` schedules a real
    asynchronous exception; ``arm_at_line`` arms the timer from a source-line
    trace hook (the hook only arms, it does not raise) so the signal can be aimed
    at a program point.
    """

    def __init__(self) -> None:
        self.landing = Landing([])
        self._armed = False
        self._timer_mode = False
        self._arm_file: str | None = None
        self._arm_func: str | None = None
        self._arm_line: int | None = None
        self._trace_active = False
        signal.signal(signal.SIGALRM, self._handler)

    def _handler(self, signum, frame):
        if not self._armed:
            return  # a late timer race after disarm: do not disturb the run
        stack = traceback.extract_stack(frame)
        self.landing.frames = [(os.path.basename(f.filename), f.name, f.lineno)
                               for f in stack[-5:]]
        self.landing.fired_at = time.monotonic()
        self._armed = False
        raise KeyboardInterrupt("async-signal")

    def _global_trace(self, frame, event, arg):
        if (event == "call" and self._arm_file
                and frame.f_code.co_filename.endswith(self._arm_file)):
            return self._local_trace
        return None

    def _local_trace(self, frame, event, arg):
        if (event == "line" and self._armed is False
                and frame.f_code.co_name == self._arm_func
                and frame.f_lineno == self._arm_line):
            self._armed = True
            self.landing.armed_at = time.monotonic()
            signal.setitimer(signal.ITIMER_REAL, self._timer_mode)
        return self._local_trace

    def arm_timer(self, delay_s: float) -> None:
        """A real asynchronous exception after ``delay_s`` seconds."""
        self._armed = True
        self.landing = Landing([], armed_at=time.monotonic())
        signal.setitimer(signal.ITIMER_REAL, delay_s)

    def arm_at_line(self, filename_suffix: str, func: str, line: int,
                    delay_s: float = 0.00003) -> None:
        """Arm the timer when execution reaches ``func:line``.

        The trace hook arms and returns; the exception itself is delivered by
        the OS timer, not raised from the hook.
        """
        self._arm_file, self._arm_func, self._arm_line = filename_suffix, func, line
        self._timer_mode = delay_s
        self.landing = Landing([], armed_at=time.monotonic())
        self._armed = True
        self._trace_active = True
        sys.settrace(self._global_trace)

    def disarm(self) -> None:
        signal.setitimer(signal.ITIMER_REAL, 0)
        self._armed = False
        if self._trace_active:
            sys.settrace(None)
            self._trace_active = False


class SigIntProbe:
    """Real SIGINT from a helper thread (equivalence check for the wait window)."""

    def __init__(self) -> None:
        import threading
        self.landing = Landing([])
        self._armed = False
        self._thread = None
        signal.signal(signal.SIGINT, self._handler)

    def _handler(self, signum, frame):
        if not self._armed:
            return  # late helper-thread signal after the trial: ignore
        stack = traceback.extract_stack(frame)
        self.landing.frames = [(os.path.basename(f.filename), f.name, f.lineno)
                               for f in stack[-4:]]
        self.landing.fired_at = time.monotonic()
        self._armed = False
        raise KeyboardInterrupt("sigint")

    def arm(self, delay_s: float) -> None:
        import threading
        self._armed = True
        self.landing = Landing([], armed_at=time.monotonic())
        pid = os.getpid()

        def run():
            time.sleep(delay_s)
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGINT)

        self._thread = threading.Thread(target=run, daemon=True)
        self._thread.start()

    def disarm(self) -> None:
        self._armed = False


class TraceRaise:
    """Deterministic synthetic KeyboardInterrupt at ``file:func:line``.

    Used to construct a state exactly (the same state is shown reachable by a
    real signal in the timing sweep).  It does not modify the library.
    """

    def __init__(self, filename_suffix: str, func: str, line: int,
                 message: str | None = None, only_first: bool = True):
        self.file, self.func, self.line = filename_suffix, func, line
        self.message = message or f"injected at {filename_suffix}:{func}:{line}"
        self.only_first = only_first
        self.fired = False
        self.landing = Landing([])
        self._active = False

    def _global(self, frame, event, arg):
        if event == "call" and frame.f_code.co_filename.endswith(self.file):
            return self._local
        return None

    def _local(self, frame, event, arg):
        if (event == "line" and not (self.only_first and self.fired)
                and frame.f_code.co_name == self.func and frame.f_lineno == self.line):
            stack = traceback.extract_stack(frame)
            self.landing.frames = [(os.path.basename(f.filename), f.name, f.lineno)
                                   for f in stack[-4:]]
            self.fired = True
            raise KeyboardInterrupt(self.message)
        return self._local

    def __enter__(self):
        self._active = True
        sys.settrace(self._global)
        return self

    def __exit__(self, *exc):
        sys.settrace(None)
        self._active = False


# --------------------------------------------------------------------------
# state classification and events
# --------------------------------------------------------------------------

def classify(client: dict, server: dict | None, retry: str | None = None,
             rows: list | None = None, target_key: str = "target",
             retry_key: str = "retry") -> str:
    """Name the state left by an interrupted transaction context.

    Names are descriptive, not claims of violation:
      clean            local and server both idle/zero
      wait_intrans     server has an open transaction, reply consumed
      pre_send         local counter advanced, server idle, no wire effect
      unread_reply     server ACTIVE / libpq busy, exchange unfinished
      other
    """
    tx = client.get("tx")
    num = client.get("num_transactions")
    closed = client.get("closed")
    if closed:
        return "closed"
    if tx == "IDLE" and num == 0:
        return "clean"
    if tx == "INTRANS" and num == 1:
        return "wait_intrans"
    if tx == "INTRANS" and num == 0:
        return "intrans_no_local_owner"
    if tx == "IDLE" and num == 1:
        return "pre_send_local_ahead"
    if tx == "ACTIVE":
        return "unread_reply"
    if tx == "INERROR":
        return "inerror"
    return f"other:{tx}:{num}"


@dataclass
class Event:
    seq: int
    t_mono: float
    name: str
    fields: dict = field(default_factory=dict)


class EventLog:
    def __init__(self) -> None:
        self.events: list[Event] = []

    def add(self, name: str, **fields) -> None:
        self.events.append(Event(len(self.events) + 1, round(time.monotonic(), 6),
                                 name, {k: sanitize(v) for k, v in fields.items()}))

    def as_list(self) -> list[dict]:
        return [{"seq": e.seq, "t_mono": e.t_mono, "event": e.name, **e.fields}
                for e in self.events]


_NOISE = ("127.0.0.1:", "dw_", "run.")


def sanitize(value):
    """Remove run-specific noise before values are compared or reported."""
    if isinstance(value, str):
        for prefix in _NOISE:
            if prefix == "127.0.0.1:":
                import re
                value = re.sub(r"127\.0\.0\.1:\d+", "127.0.0.1:<port>", value)
            else:
                value = value.replace(prefix, "<noise>")
        return value
    if isinstance(value, dict):
        return {str(k): sanitize(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [sanitize(v) for v in value]
    return value


def rows_set(rows) -> set[str]:
    return {f"{k}={v}" for k, v in rows}
