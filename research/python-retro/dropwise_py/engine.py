"""A small asyncio port of Dropwise's cancellation-injection ladder.

The Rust engine marks a target future and cancels it after its k-th `Pending`.
In asyncio the equivalent observation point is the task's k-th suspension: a
`Task` resumes at `__step`, and a step that ends with the task not done is one
yield to the event loop.  We count those steps in a pure-Python `Task`
subclass and call `Task.cancel()` right after the k-th one.

`Task.cancel()` is used rather than throwing `CancelledError` into the inner
coroutine: it is exactly what callers do in production, it cancels the future
the task is waiting on, and on Python 3.11+ it participates in
`Task.cancelling()`/`uncancel()`.  A hand-thrown exception would miss all of
that, including libraries whose cleanup checks `cancelling()`.

The judging half is a direct port: the scenario is calibrated by running it
without the target (`pre`) and with the target completed (`post`), each twice;
disagreeing samples are `Unknown`, and a cancelled trial whose observation is
neither side is `Neither`.
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses
import enum
import time
from typing import Any, Awaitable, Callable

class PureTask(asyncio.tasks._PyTask):
    """Steppable task registered with CPython 3.15's public task lookup.

    The Python Task now maintains its own registry, separate from the C
    implementation behind asyncio.current_task(). Register each step there too
    so driver cleanup and asyncio.timeout() see the actual injection task.
    This intentionally targets the current runtime, not older task layouts.
    """

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        # all_tasks() and asyncio.run() shutdown use the native registry too.
        asyncio.tasks._register_task(self)
        self.add_done_callback(asyncio.tasks._unregister_task)

    def _Task__step(self, exc=None):
        asyncio.tasks._enter_task(self._loop, self)
        try:
            super()._Task__step(exc)
        finally:
            asyncio.tasks._leave_task(self._loop, self)


class Expect(enum.Enum):
    """What the caller's contract allows after cancellation."""

    ATOMIC = "atomic"       # pre or post
    COMPLETED = "completed" # post only
    UNDONE = "undone"       # pre only


class Verdict(enum.Enum):
    PRE = "Pre"
    POST = "Post"
    MIXED = "Mixed"
    NEITHER = "Neither"
    UNKNOWN = "Unknown"


@dataclasses.dataclass
class OpResult:
    """A structured success/failure value for one ladder operation.

    Adapters must not encode failure only in a human-readable string and must
    not return ``None`` after a failed stage: either return ``OpResult`` or
    raise.  ``level_retry`` checks ``ok`` directly, so a failed stage can never
    be counted as a successful retry.
    """

    ok: bool
    stage: str
    value: Any = None
    error: str | None = None

    def __post_init__(self) -> None:
        if not self.ok and self.error is None:
            self.error = f"{self.stage} failed"


@dataclasses.dataclass
class TargetRun:
    outcome: str                 # completed | cancelled | error | timeout
    result: Any = None
    error: str | None = None
    yields: int = 0
    cut_requested: bool = False
    cut_delivered: bool = False
    seconds: float = 0.0

    @property
    def realized(self) -> bool:
        # A plan is realized when the requested cut was the thing that stopped
        # the target, not when the target finished before that suspension.
        return self.cut_requested and self.outcome in ("cancelled", "error", "timeout")


class _CutTask(PureTask):
    """Pure-Python Task that counts yields and cancels after the N-th one."""

    def __init__(self, coro: Awaitable[Any], *, cut_after: int | None = None) -> None:
        self.steps = 0
        self.yields = 0
        self.cut_after = cut_after
        self.cut_requested = False
        super().__init__(coro)

    def _Task__step(self, exc=None) -> None:  # name-mangled override
        self.steps += 1
        super()._Task__step(exc)
        if self.done():
            return
        self.yields += 1
        if (
            self.cut_after is not None
            and not self.cut_requested
            and self.yields == self.cut_after
        ):
            self.cut_requested = True
            self.cancel()


async def _await_target(
    coro: Awaitable[Any], cut_after: int | None, timeout: float
) -> TargetRun:
    started = time.monotonic()
    task = _CutTask(coro, cut_after=cut_after)
    outcome = "error"
    result = None
    error = None
    try:
        result = await asyncio.wait_for(task, timeout)
        if operation_failed(result):
            outcome = "error"
            error = f"target reported failure: {result!r}"
        else:
            outcome = "completed"
    except asyncio.CancelledError:
        outcome = "cancelled"
    except asyncio.TimeoutError:
        outcome = "timeout"
        error = f"timeout after {timeout:.1f}s"
        task.cancel()
        with contextlib.suppress(BaseException):
            await task
    except BaseException as exc:  # library error during cancellation/cleanup
        outcome = "error"
        error = f"{type(exc).__name__}: {exc}"
    return TargetRun(
        outcome=outcome,
        result=result,
        error=error,
        yields=task.yields,
        cut_requested=task.cut_requested,
        cut_delivered=task.cut_requested,
        seconds=time.monotonic() - started,
    )


def normalize(value: Any) -> Any:
    """Make observations comparable and printable without losing shape."""
    if isinstance(value, OpResult):
        return {
            "ok": value.ok,
            "stage": value.stage,
            "value": normalize(value.value),
            "error": value.error,
        }
    if isinstance(value, bytes):
        return value.decode("utf-8", "backslashreplace")
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    if isinstance(value, dict):
        return {str(k): normalize(v) for k, v in sorted(value.items(), key=lambda kv: str(kv[0]))}
    if isinstance(value, (list, tuple)):
        return [normalize(v) for v in value]
    return repr(value)


def err(exc: BaseException) -> str:
    return f"ERR {type(exc).__name__}: {exc}"


def operation_failed(value: Any) -> bool:
    """Return True iff an adapter reported a failed operation.

    New adapters return ``OpResult`` (or a dict with an explicit boolean
    ``ok`` field) or raise.  The ``ERR ...`` string is retained only for
    historical cases that predate the structured interface; it is not the
    contract for new adapters.
    """
    if isinstance(value, OpResult):
        return not value.ok
    if isinstance(value, dict) and isinstance(value.get("ok"), bool):
        return not value["ok"]
    return isinstance(value, str) and value.startswith("ERR ")


class Scenario:
    """One run's environment.  A fresh instance is made for every run."""

    async def setup(self) -> None: ...
    async def target(self) -> Any: ...
    async def same(self) -> Any: ...       # B2: same operation, same arguments
    async def different(self) -> Any: ...  # B3: different operation of same kind
    async def survivor(self) -> Any: ...   # M1: side effect through the same pool
    async def observe(self) -> Any: ...    # M1: independent observer
    async def close(self) -> None: ...


ScenarioFactory = Callable[[], Scenario]


async def _scenario_run(
    factory: ScenarioFactory,
    body: Callable[[Scenario], Awaitable[Any]],
    timeout: float,
) -> Any:
    sc = factory()
    try:
        await asyncio.wait_for(sc.setup(), timeout)
        return await body(sc)
    finally:
        with contextlib.suppress(BaseException):
            await asyncio.wait_for(sc.close(), timeout)


async def count_yields(factory: ScenarioFactory, timeout: float = 15.0) -> TargetRun:
    """Run the target once, uncancelled, to get K."""

    async def body(sc: Scenario) -> TargetRun:
        return await _await_target(sc.target(), None, timeout)

    return await _scenario_run(factory, body, timeout)


@dataclasses.dataclass
class Trial:
    plan: str
    rep: int
    target: TargetRun | None
    observation: Any
    verdict: Verdict | None = None
    violation: str | None = None
    seconds: float = 0.0


@dataclasses.dataclass
class Report:
    case: str
    level: str
    target_yields: int = 0
    pre: list[Any] = dataclasses.field(default_factory=list)
    post: list[Any] = dataclasses.field(default_factory=list)
    trials: list[Trial] = dataclasses.field(default_factory=list)
    baseline_errors: list[str] = dataclasses.field(default_factory=list)
    notes: list[str] = dataclasses.field(default_factory=list)
    exhaustive: bool = True
    seconds: float = 0.0

    def violations(self) -> list[Trial]:
        return [t for t in self.trials if t.violation]

    def unknown(self) -> list[Trial]:
        return [t for t in self.trials if t.verdict is Verdict.UNKNOWN]

    def unrealized(self) -> list[Trial]:
        return [t for t in self.trials if t.target is not None and not t.target.realized]

    def verdict_counts(self) -> dict[str, int]:
        counts: dict[str, int] = {}
        for t in self.trials:
            key = t.verdict.value if t.verdict else "-"
            counts[key] = counts.get(key, 0) + 1
        return counts


def _classify(observation: Any, pre: Any, post: Any) -> Verdict | None:
    if observation == pre:
        return Verdict.PRE
    if observation == post:
        return Verdict.POST
    return Verdict.NEITHER


def _allows(expect: Expect, verdict: Verdict) -> bool:
    if expect is Expect.ATOMIC:
        return verdict in (Verdict.PRE, Verdict.POST, Verdict.MIXED)
    if expect is Expect.COMPLETED:
        return verdict is Verdict.POST
    return verdict is Verdict.PRE


async def _observed_once(
    factory: ScenarioFactory,
    *,
    run_target: bool,
    cut_after: int | None,
    timeout: float,
) -> tuple[Any, TargetRun | None]:
    async def body(sc: Scenario) -> tuple[Any, TargetRun | None]:
        tr = None
        if run_target:
            tr = await _await_target(sc.target(), cut_after, timeout)
        try:
            survivor = normalize(await asyncio.wait_for(sc.survivor(), timeout))
        except BaseException as exc:
            survivor = err(exc)
        try:
            observer = normalize(await asyncio.wait_for(sc.observe(), timeout))
        except BaseException as exc:
            observer = err(exc)
        return {"survivor": survivor, "observer": observer}, tr

    return await _scenario_run(factory, body, timeout)


async def explore_observed(
    case: str,
    factory: ScenarioFactory,
    *,
    expect: Expect = Expect.ATOMIC,
    reps: int = 3,
    timeout: float = 15.0,
    plans: list[int] | None = None,
) -> Report:
    """M1: cancel at every suspension, judge with pre/post calibration."""
    started = time.monotonic()
    report = Report(case=case, level="M1")
    first = await count_yields(factory, timeout)
    report.target_yields = first.yields
    if not first.yields:
        report.exhaustive = False
        report.notes.append("no target suspension observed; no cancellation coverage")
    elif plans is not None and set(plans) != set(range(1, first.yields + 1)):
        report.exhaustive = False
        report.notes.append("explicit plans do not cover the discovered suspension range")
    if first.outcome != "completed":
        report.baseline_errors.append(f"uncancelled target: {first.outcome} {first.error or ''}")
        report.exhaustive = False
        report.seconds = time.monotonic() - started
        return report

    # Two samples per side; disagreement makes every trial Unknown.
    for _ in range(2):
        obs, _ = await _observed_once(
            factory, run_target=False, cut_after=None, timeout=timeout
        )
        report.pre.append(obs)
    for _ in range(2):
        obs, target = await _observed_once(
            factory, run_target=True, cut_after=None, timeout=timeout
        )
        if target is None or target.outcome != "completed":
            report.baseline_errors.append(f"post calibration target failed: {target!r}")
            report.exhaustive = False
            report.seconds = time.monotonic() - started
            return report
        report.post.append(obs)
    calibration_ok = report.pre[0] == report.pre[1] and report.post[0] == report.post[1]
    if not calibration_ok:
        report.notes.append("pre/post samples disagreed; verdicts are Unknown")
    if report.pre[0] == report.post[0]:
        report.notes.append("pre == post (degenerate observation)")

    for k in range(1, report.target_yields + 1) if plans is None else plans:
        for rep in range(1, reps + 1):
            trial_started = time.monotonic()
            obs, tr = await _observed_once(
                factory, run_target=True, cut_after=k, timeout=timeout
            )
            trial = Trial(
                plan=f"pending#{k}",
                rep=rep,
                target=tr,
                observation=obs,
                seconds=time.monotonic() - trial_started,
            )
            if tr is None or not tr.realized:
                trial.verdict = Verdict.UNKNOWN
                report.exhaustive = False
            elif not calibration_ok:
                trial.verdict = Verdict.UNKNOWN
            else:
                trial.verdict = _classify(obs, report.pre[0], report.post[0])
                if not _allows(expect, trial.verdict):
                    trial.violation = (
                        f"{trial.verdict.value} under {expect.value}: observed {obs!r}, "
                        f"pre={report.pre[0]!r}, post={report.post[0]!r}"
                    )
            report.trials.append(trial)
    report.seconds = time.monotonic() - started
    return report


async def _cut_only(factory: ScenarioFactory, cut_after: int, timeout: float) -> tuple[TargetRun, float]:
    started = time.monotonic()

    async def body(sc: Scenario) -> TargetRun:
        return await _await_target(sc.target(), cut_after, timeout)

    return await _scenario_run(factory, body, timeout), time.monotonic() - started


async def level_b1(
    case: str,
    factory: ScenarioFactory,
    *,
    reps: int = 3,
    timeout: float = 15.0,
    plans: list[int] | None = None,
) -> Report:
    """Cancel and stop.  A violation is an escaping error or a timeout."""
    started = time.monotonic()
    report = Report(case=case, level="B1")
    first = await count_yields(factory, timeout)
    report.target_yields = first.yields
    if not first.yields:
        report.exhaustive = False
        report.notes.append("no target suspension observed; no cancellation coverage")
    elif plans is not None and set(plans) != set(range(1, first.yields + 1)):
        report.exhaustive = False
        report.notes.append("explicit plans do not cover the discovered suspension range")
    if first.outcome != "completed":
        report.baseline_errors.append(f"uncancelled target: {first.outcome} {first.error or ''}")
        report.exhaustive = False
        report.seconds = time.monotonic() - started
        return report
    for k in range(1, report.target_yields + 1) if plans is None else plans:
        for rep in range(1, reps + 1):
            tr, secs = await _cut_only(factory, k, timeout)
            trial = Trial(plan=f"pending#{k}", rep=rep, target=tr, observation=None, seconds=secs)
            if not tr.realized:
                report.exhaustive = False
            elif tr.outcome in ("error", "timeout"):
                trial.violation = f"after {plan(k)}: {tr.outcome} {tr.error or ''}"
            report.trials.append(trial)
    report.seconds = time.monotonic() - started
    return report


async def _survivor_once(
    factory: ScenarioFactory,
    *,
    cut_after: int,
    retry: str,
    timeout: float,
) -> tuple[TargetRun, Any, float]:
    started = time.monotonic()

    async def body(sc: Scenario) -> tuple[TargetRun, Any]:
        tr = await _await_target(sc.target(), cut_after, timeout)
        op = sc.same() if retry == "same" else sc.different()
        try:
            value = normalize(await asyncio.wait_for(op, timeout))
        except BaseException as exc:
            value = err(exc)
        return tr, value

    return (*await _scenario_run(factory, body, timeout), time.monotonic() - started)


async def level_retry(
    case: str,
    factory: ScenarioFactory,
    *,
    level: str,
    retry: str,
    reps: int = 3,
    timeout: float = 15.0,
    plans: list[int] | None = None,
) -> Report:
    """B2 (same operation) / B3 (different operation); only `not an error` counts."""
    started = time.monotonic()
    report = Report(case=case, level=level)
    first = await count_yields(factory, timeout)
    report.target_yields = first.yields
    if not first.yields:
        report.exhaustive = False
        report.notes.append("no target suspension observed; no cancellation coverage")
    elif plans is not None and set(plans) != set(range(1, first.yields + 1)):
        report.exhaustive = False
        report.notes.append("explicit plans do not cover the discovered suspension range")
    if first.outcome != "completed":
        report.baseline_errors.append(f"uncancelled target: {first.outcome} {first.error or ''}")
        report.exhaustive = False
        report.seconds = time.monotonic() - started
        return report
    for k in range(1, report.target_yields + 1) if plans is None else plans:
        for rep in range(1, reps + 1):
            tr, value, secs = await _survivor_once(
                factory, cut_after=k, retry=retry, timeout=timeout
            )
            trial = Trial(
                plan=f"pending#{k}", rep=rep, target=tr, observation=value, seconds=secs
            )
            if not tr.realized:
                report.exhaustive = False
            elif operation_failed(value):
                trial.violation = f"after {plan(k)}: {value!r}"
            report.trials.append(trial)
    report.seconds = time.monotonic() - started
    return report


def plan(k: int) -> str:
    return f"pending#{k}"


def report_lines(report: Report) -> list[str]:
    out = [
        f"case={report.case} level={report.level} yields={report.target_yields} "
        f"trials={len(report.trials)} violations={len(report.violations())} "
        f"unknown={len(report.unknown())} unrealized={len(report.unrealized())} "
        f"seconds={report.seconds:.2f}"
    ]
    for note in report.notes:
        out.append(f"note: {note}")
    for error in report.baseline_errors:
        out.append(f"baseline_error: {error}")
    if report.pre:
        out.append(f"pre={report.pre!r}")
        out.append(f"post={report.post!r}")
    for t in report.trials:
        status = "VIOLATION" if t.violation else "ok"
        verdict = t.verdict.value if t.verdict else "-"
        tr = t.target
        target = (
            f"{tr.outcome}/y{tr.yields}/realized={tr.realized}"
            if tr is not None
            else "skipped"
        )
        detail = t.violation or ""
        out.append(
            f"  plan={t.plan} rep={t.rep} target={target} verdict={verdict} "
            f"obs={t.observation!r} {status} {detail}"
        )
    counts = report.verdict_counts()
    if counts:
        out.append(f"verdicts={counts}")
    return out


def print_report(report: Report) -> None:
    for line in report_lines(report):
        print(line, flush=True)
