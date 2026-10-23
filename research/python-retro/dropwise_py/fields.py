"""Field-level observation contracts for the cancellation ladder.

The engine's original observation is one value: the pair
``{"survivor": ..., "observer": ...}`` from M1, compared against a pre and a
post calibration blob.  That has two blind spots which this module addresses
without changing the legacy API:

* **Independent effects are fused.**  If a cancelled operation half-completed
  and then a survivor effect completed, the blob is neither ``pre`` nor
  ``post`` and the report says ``Neither`` even when the caller's contract
  (atomic for the cancelled operation, completed for the survivor) is
  satisfied.  Conversely a blob that equals ``post`` can hide a field that is
  degenerate (identical in pre and post), i.e. the scenario never observed it.

* **No coverage statement per field.**  ``pre == post`` is only a note today.
  A required effect that is invisible in both calibrations must make the run
  *uncovered*, not clean.

A contract is a list of named :class:`Field` values.  Each observation is a
dict (nested dicts are flattened to dotted names); each field is classified as
``EFFECT`` (compared to pre/post) or ``STATE`` (an invariant that must hold in
both calibrations and in the trial).  Each effect carries its own
:class:`~dropwise_py.engine.Expect`, so a contract can say "the cancelled write
may be pre or post, but the later survivor write must be post" in one run.

Nothing here is asyncio-specific: :func:`judge_trial` works on plain dicts and
is reused by the synchronous psycopg harness.
"""

from __future__ import annotations

import dataclasses
import enum
import time
from typing import Any, Iterable

from .engine import Expect, ScenarioFactory, TargetRun, normalize

__all__ = [
    "Field",
    "FieldContract",
    "FieldContractError",
    "FieldJudgment",
    "FieldKind",
    "FieldReport",
    "FieldStatus",
    "FieldTrial",
    "FieldVerdict",
    "calibration",
    "explore_fields",
    "flatten_observation",
    "judge_trial",
]


class FieldKind(enum.Enum):
    EFFECT = "effect"   # externally visible effect, compared to pre/post
    STATE = "state"     # invariant that must hold in both calibrations


class FieldStatus(enum.Enum):
    PRE = "Pre"
    POST = "Post"
    MIXED = "Mixed"
    HOLDS = "Holds"
    NEITHER = "Neither"
    UNKNOWN = "Unknown"
    DEGENERATE = "Degenerate"     # effect identical in pre and post
    UNCOVERED = "Uncovered"       # required field absent from the observation
    VIOLATION = "Violation"       # state guard failed or per-field expect failed


class FieldContractError(ValueError):
    pass


@dataclasses.dataclass(frozen=True)
class Field:
    name: str
    kind: FieldKind = FieldKind.EFFECT
    expect: Expect = Expect.ATOMIC

    def __post_init__(self) -> None:
        if not self.name or self.name.startswith(".") or self.name.endswith("."):
            raise FieldContractError(f"invalid field name {self.name!r}")


@dataclasses.dataclass(frozen=True)
class FieldContract:
    fields: tuple[Field, ...]

    def __post_init__(self) -> None:
        if not self.fields:
            raise FieldContractError("a contract needs at least one field")
        names = [f.name for f in self.fields]
        if len(names) != len(set(names)):
            raise FieldContractError(f"duplicate field names: {names}")
        if not any(f.kind is FieldKind.EFFECT for f in self.fields):
            raise FieldContractError(
                "a contract needs at least one EFFECT field; a state-only check "
                "cannot decide pre/post and belongs to level_b1"
            )

    @property
    def names(self) -> tuple[str, ...]:
        return tuple(f.name for f in self.fields)

    def get(self, name: str) -> Field:
        for f in self.fields:
            if f.name == name:
                return f
        raise KeyError(name)


def flatten_observation(value: Any, prefix: str = "") -> dict[str, Any]:
    """Flatten nested dicts to dotted names; leaves are normalized.

    A non-dict top-level value is a single field ``value`` so that adapters can
    migrate incrementally.
    """
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        for key, val in value.items():
            name = f"{prefix}.{key}" if prefix else str(key)
            leaves = flatten_observation(val, name) if isinstance(val, dict) else {name: normalize(val)}
            duplicate = out.keys() & leaves.keys()
            if duplicate:
                raise FieldContractError(f"ambiguous dotted observation fields: {sorted(duplicate)}")
            out.update(leaves)
        return out
    return {prefix or "value": normalize(value)}


def _stable(values: list[Any]) -> tuple[Any, bool]:
    if not values:
        return None, False
    first = values[0]
    return first, all(v == first for v in values[1:])


@dataclasses.dataclass
class FieldVerdict:
    name: str
    kind: FieldKind
    status: FieldStatus
    value: Any
    pre: Any
    post: Any
    detail: str = ""

    @property
    def ok(self) -> bool:
        """True iff this field alone satisfies its expectation and is covered."""
        if self.kind is FieldKind.STATE:
            return self.status is FieldStatus.HOLDS
        return self.status in (FieldStatus.PRE, FieldStatus.POST, FieldStatus.MIXED)


@dataclasses.dataclass
class FieldJudgment:
    fields: list[FieldVerdict]
    status: FieldStatus

    def by_name(self) -> dict[str, FieldVerdict]:
        return {f.name: f for f in self.fields}

    @property
    def coverage_complete(self) -> bool:
        return all(f.status not in (FieldStatus.UNCOVERED, FieldStatus.DEGENERATE,
                                    FieldStatus.UNKNOWN) for f in self.fields)

    @property
    def violations(self) -> list[FieldVerdict]:
        return [f for f in self.fields if f.status in (FieldStatus.VIOLATION, FieldStatus.NEITHER)]


def _precedence(statuses: Iterable[FieldStatus]) -> FieldStatus:
    """Combine per-field statuses into one report status.

    A definite badness outranks a coverage gap; a coverage gap outranks
    ``Mixed``/``Pre``/``Post`` so a clean-looking run cannot hide an unobserved
    required effect.
    """
    present = set(statuses)
    for status in (FieldStatus.VIOLATION, FieldStatus.UNKNOWN, FieldStatus.NEITHER,
                   FieldStatus.UNCOVERED, FieldStatus.DEGENERATE):
        if status in present:
            return status
    if FieldStatus.PRE in present and FieldStatus.POST in present:
        return FieldStatus.MIXED
    if FieldStatus.POST in present:
        return FieldStatus.POST
    if FieldStatus.PRE in present:
        return FieldStatus.PRE
    if FieldStatus.HOLDS in present:
        return FieldStatus.HOLDS
    return FieldStatus.UNKNOWN


def judge_trial(
    contract: FieldContract,
    pre_samples: list[dict[str, Any]],
    post_samples: list[dict[str, Any]],
    observation: Any,
) -> FieldJudgment:
    """Judge one flattened observation against two stable calibrations."""
    obs = flatten_observation(observation)
    pre_samples = [flatten_observation(s) for s in pre_samples]
    post_samples = [flatten_observation(s) for s in post_samples]
    verdicts: list[FieldVerdict] = []
    for field in contract.fields:
        present = field.name in obs
        value = obs.get(field.name)
        pre_vals = [s.get(field.name) for s in pre_samples]
        post_vals = [s.get(field.name) for s in post_samples]
        pre, pre_stable = _stable(pre_vals)
        post, post_stable = _stable(post_vals)
        if not present:
            verdicts.append(FieldVerdict(
                field.name, field.kind, FieldStatus.UNCOVERED, None, pre, post,
                "required field absent from the observation"))
            continue
        if any(field.name not in sample for sample in pre_samples + post_samples):
            verdicts.append(FieldVerdict(
                field.name, field.kind, FieldStatus.UNCOVERED, value, pre, post,
                "field absent from calibration; missing is not an observed None"))
            continue
        if not pre_stable or not post_stable:
            verdicts.append(FieldVerdict(
                field.name, field.kind, FieldStatus.UNKNOWN, value, pre, post,
                "calibration samples disagree"))
            continue
        if field.kind is FieldKind.STATE:
            if pre != post:
                verdicts.append(FieldVerdict(
                    field.name, field.kind, FieldStatus.UNKNOWN, value, pre, post,
                    "state differs between the two calibrations"))
            elif value == pre:
                verdicts.append(FieldVerdict(
                    field.name, field.kind, FieldStatus.HOLDS, value, pre, post))
            else:
                verdicts.append(FieldVerdict(
                    field.name, field.kind, FieldStatus.VIOLATION, value, pre, post,
                    f"state guard failed: {value!r} != calibrated {pre!r}"))
            continue
        # EFFECT
        if pre == post:
            verdicts.append(FieldVerdict(
                field.name, field.kind, FieldStatus.DEGENERATE, value, pre, post,
                "effect identical in pre and post calibration; no discriminating power"))
            continue
        if value == pre:
            status = FieldStatus.PRE
        elif value == post:
            status = FieldStatus.POST
        else:
            verdicts.append(FieldVerdict(
                field.name, field.kind, FieldStatus.NEITHER, value, pre, post,
                f"value {value!r} matches neither pre {pre!r} nor post {post!r}"))
            continue
        allowed = (
            field.expect is Expect.ATOMIC
            or (field.expect is Expect.COMPLETED and status is FieldStatus.POST)
            or (field.expect is Expect.UNDONE and status is FieldStatus.PRE)
        )
        if allowed:
            verdicts.append(FieldVerdict(field.name, field.kind, status, value, pre, post))
        else:
            verdicts.append(FieldVerdict(
                field.name, field.kind, FieldStatus.VIOLATION, value, pre, post,
                f"{status.value} not allowed under {field.expect.value}"))
    return FieldJudgment(verdicts, _precedence(v.status for v in verdicts))


@dataclasses.dataclass
class FieldTrial:
    plan: str
    rep: int
    target: TargetRun | None
    observation: dict[str, Any]
    judgment: FieldJudgment | None = None
    seconds: float = 0.0


@dataclasses.dataclass
class FieldReport:
    case: str
    level: str = "F1"
    contract: FieldContract | None = None
    target_yields: int = 0
    pre: list[dict[str, Any]] = dataclasses.field(default_factory=list)
    post: list[dict[str, Any]] = dataclasses.field(default_factory=list)
    trials: list[FieldTrial] = dataclasses.field(default_factory=list)
    baseline_errors: list[str] = dataclasses.field(default_factory=list)
    notes: list[str] = dataclasses.field(default_factory=list)
    exhaustive: bool = True
    seconds: float = 0.0

    def violations(self) -> list[FieldTrial]:
        return [t for t in self.trials
                if t.judgment and t.judgment.status in
                (FieldStatus.VIOLATION, FieldStatus.NEITHER)]

    def uncovered(self) -> list[FieldTrial]:
        return [t for t in self.trials
                if t.judgment and t.judgment.status in
                (FieldStatus.UNCOVERED, FieldStatus.DEGENERATE)]

    def status_counts(self) -> dict[str, int]:
        counts: dict[str, int] = {}
        for t in self.trials:
            key = t.judgment.status.value if t.judgment else "-"
            counts[key] = counts.get(key, 0) + 1
        return counts

    def field_counts(self) -> dict[str, dict[str, int]]:
        out: dict[str, dict[str, int]] = {}
        for t in self.trials:
            if not t.judgment:
                continue
            for f in t.judgment.fields:
                slot = out.setdefault(f.name, {})
                slot[f.status.value] = slot.get(f.status.value, 0) + 1
        return out


def calibration(
    contract: FieldContract, pre_samples: list[dict[str, Any]],
    post_samples: list[dict[str, Any]],
) -> dict[str, dict[str, Any]]:
    """Per-field calibration facts, useful in a report even without trials."""
    out = {}
    pre_samples = [flatten_observation(s) for s in pre_samples]
    post_samples = [flatten_observation(s) for s in post_samples]
    for field in contract.fields:
        pre, pre_stable = _stable([s.get(field.name) for s in pre_samples])
        post, post_stable = _stable([s.get(field.name) for s in post_samples])
        covered = bool(pre_samples and post_samples) and all(
            field.name in sample for sample in pre_samples + post_samples)
        stable = covered and pre_stable and post_stable
        out[field.name] = {
            "kind": field.kind.value,
            "expect": field.expect.value,
            "pre": pre,
            "post": post,
            "covered": covered,
            "stable": stable,
            "discriminates": stable and field.kind is FieldKind.EFFECT and pre != post,
        }
    return out


async def explore_fields(
    case: str,
    factory: ScenarioFactory,
    contract: FieldContract,
    *,
    reps: int = 3,
    timeout: float = 15.0,
    plans: list[int] | None = None,
) -> FieldReport:
    """M1 with a field contract.

    The scenario lifecycle and cancellation plans are the engine's; only the
    judging changes.  Calibration is two pre (target not run) and two post
    (target completed) runs, flattened once per run.
    """
    # Imported lazily to keep this module importable without the asyncio driver
    # internals (the synchronous harness only needs judge_trial).
    from .engine import _observed_once, _validate_search, count_yields

    _validate_search(reps, timeout, plans)
    started = time.monotonic()
    report = FieldReport(case=case, contract=contract)
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

    for _ in range(2):
        obs, _ = await _observed_once(
            factory, run_target=False, cut_after=None, timeout=timeout)
        report.pre.append(flatten_observation(obs))
    for _ in range(2):
        obs, target = await _observed_once(
            factory, run_target=True, cut_after=None, timeout=timeout)
        if target is None or target.outcome != "completed":
            report.baseline_errors.append(f"post calibration target failed: {target!r}")
            report.exhaustive = False
            report.seconds = time.monotonic() - started
            return report
        report.post.append(flatten_observation(obs))

    if report.pre[0] == report.post[0]:
        report.notes.append("pre == post for the whole observation; every effect is degenerate")
    for name, info in calibration(contract, report.pre, report.post).items():
        if not info["stable"]:
            report.notes.append(f"{name}: calibration unstable")
        if info["kind"] == "effect" and not info["discriminates"]:
            report.notes.append(f"{name}: effect does not discriminate pre from post")

    for k in range(1, report.target_yields + 1) if plans is None else plans:
        for rep in range(1, reps + 1):
            trial_started = time.monotonic()
            obs, tr = await _observed_once(
                factory, run_target=True, cut_after=k, timeout=timeout)
            flat = flatten_observation(obs)
            trial = FieldTrial(
                plan=f"pending#{k}", rep=rep, target=tr, observation=flat,
                seconds=time.monotonic() - trial_started)
            if tr is None or not tr.realized:
                report.exhaustive = False
                trial.judgment = None
            else:
                trial.judgment = judge_trial(contract, report.pre, report.post, flat)
            report.trials.append(trial)

    report.seconds = time.monotonic() - started
    if any(not t.judgment or not t.judgment.coverage_complete for t in report.trials):
        report.notes.append("some trials had uncovered or degenerate required fields")
    return report
