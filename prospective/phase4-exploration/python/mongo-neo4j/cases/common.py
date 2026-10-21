"""Shared helpers for the phase-4 round-3 MongoDB / Neo4j Python cases.

The Dropwise Python engine (`dropwise_py`) and the delay proxy
(`cases/common.py`) are imported read-only from `research/python-retro/`.
Nothing in that directory is modified.

Observation convention: every operation that can hang is awaited through
`bound()`, which turns the outcome into one of

    ("ok", value) | ("timeout", "2.0s") | ("err", "ExceptionType: text")

so a hang becomes a comparable value instead of killing the trial.  Error
text is sanitised (unique collection/label name, proxy port, ...) before it
enters an observation; otherwise every run would hash differently and
calibration would be `Unknown` for a harness reason.
"""

from __future__ import annotations

import asyncio
import contextlib
import importlib.util
import itertools
import os
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve()


def _find_repo(start: Path) -> Path:
    for parent in start.parents:
        if (parent / "research" / "python-retro" / "dropwise_py").is_dir():
            return parent
    raise RuntimeError(f"cannot locate repo root from {start}")


REPO = _find_repo(HERE)
RETRO = Path(os.environ.get("DROPWISE_RETRO", REPO / "research" / "python-retro"))
if str(RETRO) not in sys.path:
    sys.path.insert(0, str(RETRO))

from dropwise_py.engine import OpResult  # noqa: E402


def _load_retro_proxy():
    """Load DelayProxy from the read-only retro helper by path.

    Importing `cases.common` would collide with this batch's own `cases`
    package, so the retro module is loaded under a private name.
    """
    path = RETRO / "cases" / "common.py"
    spec = importlib.util.spec_from_file_location("dw_retro_common", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.DelayProxy


DelayProxy = _load_retro_proxy()

BOUND = 2.0          # every individual operation
DELAY = 0.05         # server->client delay per forwarded chunk (proxy)

_seq = itertools.count(1)
_PORT_RE = re.compile(r"(127\.0\.0\.1|localhost):\d+")
_NAME_RE = re.compile(r"\b[Dd]w_?[Rr]3[_\w]*")


def unique_name(prefix: str = "dw_r3") -> str:
    """A fresh collection/label name per scenario instance (unique per run)."""
    return f"{prefix}_{os.getpid()}_{next(_seq)}"


async def bound(coro, timeout: float = BOUND):
    """Await one operation, converting hang/error into a comparable value.

    `CancelledError` is never swallowed: the engine may cancel the survivor
    (or the runner may cancel the scenario) and that must keep propagating.
    """
    try:
        value = await asyncio.wait_for(coro, timeout)
        return ("ok", value)
    except asyncio.CancelledError:
        raise
    except TimeoutError:
        return ("timeout", f"{timeout:.1f}s")
    except BaseException as exc:  # library error: keep class and text
        return ("err", f"{type(exc).__name__}: {exc}")


def sanitize(value, name: str | None = None):
    """Replace run-specific noise (unique name, loopback port) in values."""
    if isinstance(value, str):
        if name:
            value = value.replace(name, "<name>")
        value = _NAME_RE.sub("<name>", value)
        return _PORT_RE.sub("127.0.0.1:<port>", value)
    if isinstance(value, dict):
        return {str(k): sanitize(v, name) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [sanitize(v, name) for v in value]
    if isinstance(value, bytes):
        return sanitize(value.decode("utf-8", "backslashreplace"), name)
    if isinstance(value, (int, float, bool)) or value is None:
        return value
    return sanitize(repr(value), name)


def err_text(exc: BaseException) -> str:
    return f"{type(exc).__name__}: {exc}"


def observation(survivor, observer, name: str):
    return sanitize({"survivor": survivor, "observer": observer}, name)


def add_service_args(parser) -> None:
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--levels", default="B1,B2,B3,M1")
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=BOUND)
    parser.add_argument("--round", type=int, default=0)
    parser.add_argument(
        "--mode", default="txn", choices=["txn", "plain"],
        help="session/transaction unit or ordinary request unit",
    )
    parser.add_argument(
        "--diag", action="store_true", help="reuse + pre/post calibration details"
    )
    parser.add_argument(
        "--probe", default=None,
        help="comma-separated yields to probe with full detail (no ladder)",
    )


def compact(report) -> str:
    return (
        f"SUMMARY level={report.level} yields={report.target_yields} "
        f"trials={len(report.trials)} violations={len(report.violations())} "
        f"unknown={len(report.unknown())} unrealized={len(report.unrealized())} "
        f"seconds={report.seconds:.2f}"
    )


async def close_quiet(awaitable, timeout: float = BOUND) -> str:
    """Bounded cleanup that does not mask the caller's cancellation."""
    try:
        await asyncio.wait_for(awaitable, timeout)
        return "ok"
    except asyncio.CancelledError:
        raise
    except TimeoutError:
        return "timeout"
    except BaseException as exc:
        return err_text(exc)


def suppress_quiet(fn) -> None:
    with contextlib.suppress(BaseException):
        fn()


def bad_paths(value, path: str = "value") -> list[str]:
    """Find nested ("err"|"timeout", ...) results inside a retry value."""
    if (
        isinstance(value, tuple)
        and len(value) == 2
        and isinstance(value[0], str)
        and value[0] in ("err", "timeout")
    ):
        return [f"{path}={value!r}"]
    found: list[str] = []
    if isinstance(value, dict):
        for k, v in value.items():
            found += bad_paths(v, f"{path}.{k}")
    elif isinstance(value, (list, tuple)):
        for i, v in enumerate(value):
            found += bad_paths(v, f"{path}[{i}]")
    return found


def require_clean(value):
    """B2/B3: a retry that is semantically an error/timeout must not pass."""
    bad = bad_paths(value)
    if bad:
        raise RuntimeError("retry not clean: " + "; ".join(bad))
    return value


def op_from(stage: str, stages: dict, value) -> OpResult:
    """Structured result for a ladder operation.

    `stages` maps a critical stage name to its raw `bound()` outcome.  The
    first err/timeout makes the whole operation `ok=False`, so `level_retry`
    can never count it as a successful retry, and an M1 survivor failure
    cannot compare equal to pre/post.
    """
    failed = [
        (name, res) for name, res in stages.items()
        if isinstance(res, tuple) and len(res) == 2
        and isinstance(res[0], str) and res[0] in ("err", "timeout")
    ]
    if failed:
        name, res = failed[0]
        return OpResult(False, f"{stage}.{name}", value=value,
                        error=f"{res[0]}: {res[1]}")
    return OpResult(True, stage, value=value)
