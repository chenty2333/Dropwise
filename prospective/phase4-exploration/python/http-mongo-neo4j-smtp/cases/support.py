"""Shared support for the round-3 Python batch.

The asyncio judging engine and the delay proxy are reused read-only from
``research/python-retro``: they are imported from that checkout, never copied
into or modified there.  This module only adds batch-local helpers (bounded
operations, per-instance token normalisation, fuller target reporting).
"""

from __future__ import annotations

import asyncio
import contextlib
import importlib.util
import sys
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE.parent
# .../prospective/phase4-exploration/python/http-mongo-neo4j-smtp
REPO = BASE.parents[3]
RETRO = REPO / "research" / "python-retro"
assert (RETRO / "dropwise_py" / "engine.py").is_file(), RETRO

if str(RETRO) not in sys.path:
    sys.path.insert(0, str(RETRO))

from dropwise_py import (  # noqa: E402,F401  (re-exported for the cases)
    Expect,
    Scenario,
    Verdict,
    count_yields,
    explore_observed,
    level_b1,
    level_retry,
    normalize,
    print_report,
    report_lines,
)

# Reuse cases/common.py by path: this batch's own package is also called
# ``cases``, so importing it by name would resolve to the wrong module.
_spec = importlib.util.spec_from_file_location("retro_common", RETRO / "cases" / "common.py")
_retro_common = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(_retro_common)

DelayProxy = _retro_common.DelayProxy
base_parser = _retro_common.base_parser
compact = _retro_common.compact

#: Every target/survivor/observer operation is bounded; a hang becomes a value.
BOUND = 2.0
#: The engine's own per-run timeout: setup plus bounded operations.
ENGINE_TIMEOUT = 15.0


def unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:12]}"


async def bounded(awaitable, label: str, timeout: float = BOUND):
    """Await an operation with a bound; return a comparable error string.

    ``CancelledError`` is re-raised: the runner's cancellation must propagate
    (python-retro RESULTS 8.2).  Everything else, including the timeout, is
    turned into a concrete ``ERR ...`` string that the observation carries.
    """
    try:
        return await asyncio.wait_for(awaitable, timeout)
    except asyncio.CancelledError:
        raise
    except BaseException as exc:  # noqa: BLE001 - the text is the observation
        return f"ERR {label} {type(exc).__name__}: {exc}"


def norm(value, token: str):
    """Replace this run's dynamic token with a stable marker, recursively."""
    if isinstance(value, str):
        return value.replace(token, "<run>")
    if isinstance(value, dict):
        return {norm(k, token): norm(v, token) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [norm(v, token) for v in value]
    return value


def print_full_report(report) -> None:
    """Engine report plus concrete target outcome/error/result per trial.

    The engine's own line only prints ``outcome/yields/realized``; for a
    bounded target the concrete text is in ``TargetRun.result`` and would
    otherwise be lost from the log.
    """
    print_report(report)
    for trial in report.trials:
        target = trial.target
        if target is None:
            continue
        detail = target.result if target.result is not None else target.error
        if detail is None:
            detail = ""
        text = repr(detail)
        if len(text) > 400:
            text = text[:400] + "..."
        print(
            f"  target-detail plan={trial.plan} rep={trial.rep} "
            f"outcome={target.outcome} yields={target.yields} "
            f"realized={target.realized} detail={text}",
            flush=True,
        )


def add_http_arg(parser):
    parser.add_argument(
        "--http",
        default=None,
        help="observer/base URL (GreenMail REST or direct HTTP server)",
    )
    return parser


async def close_quietly(*aws) -> None:
    """Best-effort bounded cleanup helper (each callable returns an awaitable)."""
    for aw in aws:
        if aw is None:
            continue
        with contextlib.suppress(BaseException):
            await aw
