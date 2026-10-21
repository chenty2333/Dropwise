#!/usr/bin/env python3
"""Summarise the async-smtp exploration logs.

Reads only files under logs/ and prints a Markdown fragment. No network and no
changes outside the log directory.
"""

from __future__ import annotations

import collections
import os
import re
import sys

BASE = os.path.dirname(os.path.abspath(__file__))
LOGS = os.path.join(BASE, "logs")


def read(path: str) -> str:
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            return handle.read()
    except FileNotFoundError:
        return ""


def ladder_fields(line: str) -> dict[str, str]:
    _, rest = line.split("]:", 1)
    fields: dict[str, str] = {}
    for part in rest.replace("=", "= ").split():
        if "=" in part:
            key, value = part.split("=", 1)
            fields[key] = value.strip()
    # Re-parse key=value tokens that contain no spaces (all LADDER fields do).
    fields = {}
    for token in rest.split():
        if token.count("=") == 1:
            key, value = token.split("=", 1)
            fields[key] = value
    return fields


def summarize_level(level: str, rep: int) -> dict[str, object]:
    out = read(os.path.join(LOGS, f"level-{level}-rep{rep}.out.log"))
    err = read(os.path.join(LOGS, f"level-{level}-rep{rep}.err.log"))
    ladder = [line for line in out.splitlines() if line.startswith(f"LADDER {level} ")]
    violations = [
        line for line in out.splitlines() if line.startswith("LADDER-VIOLATION")
    ]
    bad = [
        line for line in out.splitlines() if line.startswith("LADDER-ERROR")
    ]
    fields = ladder_fields(ladder[0]) if ladder else {}
    result: dict[str, object] = {
        "level": level,
        "rep": rep,
        "status": read(os.path.join(LOGS, f"level-{level}-rep{rep}.status")).strip(),
        "ladder": ladder[0] if ladder else "(missing)",
        "fields": fields,
        "violations": violations,
        "errors": bad,
        "runs": [],
        "hang_count": 0,
        "target_cancelled": 0,
        "target_completed": 0,
        "survivor_codes": collections.Counter(),
        "survivor_errors": collections.Counter(),
        "observations": [],
        "plans": [],
    }
    for line in out.splitlines():
        if line.startswith(f"{level}-RUN "):
            result["runs"].append(line)
            if "target=cancelled" in line:
                result["target_cancelled"] += 1
            elif "target=Ok(" in line:
                result["target_completed"] += 1
            match = re.search(r"survivor=(.*?) connections=", line)
            if match:
                survivor = match.group(1)
                code = re.search(r"Ok\(code=(\d+)", survivor)
                if code:
                    result["survivor_codes"][code.group(1)] += 1
                elif survivor.startswith("Err("):
                    result["survivor_errors"][survivor] += 1
        if line.startswith("M1-OBS "):
            result["observations"].append(line)
        if line.startswith(f"PHASE {level} survivor_start"):
            result["survivor_starts"] = result.get("survivor_starts", 0) + 1
        if line.startswith(f"PHASE {level} survivor_done"):
            result["survivor_dones"] = result.get("survivor_dones", 0) + 1

    # Per-plan verdicts and observed values, from Report::diagnostics.
    plan = None
    verdict = ""
    comparison = ""
    invariant = ""
    for line in out.splitlines() + [""]:
        if line.startswith("plan "):
            if plan is not None:
                result["plans"].append(
                    {"plan": plan, "verdict": verdict, "invariant": invariant, "comparison": comparison}
                )
            plan = line[len("plan "):]
            verdict = comparison = invariant = ""
        elif plan is not None:
            if line.startswith("  verdict: "):
                verdict = line[len("  verdict: "):]
            elif line.startswith("  comparison: "):
                comparison = line[len("  comparison: "):]
            elif line.startswith("  invariant: "):
                invariant = line[len("  invariant: "):]
    if plan is not None:
        result["plans"].append(
            {"plan": plan, "verdict": verdict, "invariant": invariant, "comparison": comparison}
        )
    starts = result.get("survivor_starts", 0)
    dones = result.get("survivor_dones", 0)
    timed_out_plans = sum(1 for plan in result["plans"] if "did not finish" in plan["invariant"])
    if level == "M1":
        result["hang_count"] = timed_out_plans
    else:
        result["hang_count"] = max(0, starts - dones)
    result["stderr_tail"] = "\n".join(err.strip().splitlines()[-3:])
    return result


def main() -> int:
    levels = [part for part in sys.argv[1:]] or ["B1", "B2", "B3", "M1"]
    reps = [1, 2, 3]
    rows = [summarize_level(level, rep) for level in levels for rep in reps]

    print("### Ladder table (from logs/LADDER.txt and per-rep logs)\n")
    print("| level | rep | plans | violations | unrealized | unsettled | baseline_errors | status | B2/B3 survivor values | M1 verdicts | hang? |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for row in rows:
        fields = row["fields"]
        values = ""
        if row["level"] in ("B2", "B3"):
            codes = ", ".join(f"Ok({code})" for code, count in sorted(row["survivor_codes"].items()) for _ in range(count))
            errors = ", ".join(row["survivor_errors"].keys())
            values = "; ".join(part for part in (codes, errors) if part)
        verdicts = ""
        if row["level"] == "M1":
            verdicts = " ".join(
                f"{key}={fields.get(key, '?')}"
                for key in ("pre", "post", "mixed", "neither", "unknown")
            )
        print(
            f"| {row['level']} | {row['rep']} | {fields.get('plans','?')} | {fields.get('violations','?')} | "
            f"{fields.get('unrealized','?')} | {fields.get('unsettled','?')} | {fields.get('baseline_errors','?')} | "
            f"{row['status'] or '?'} | {values or '-'} | {verdicts or '-'} | {row['hang_count']} |"
        )

    print("\n### Per-plan M1 observations\n")
    for row in rows:
        if row["level"] != "M1":
            continue
        print(f"**M1 rep {row['rep']}**\n")
        for plan in row["plans"]:
            observed = plan["comparison"]
            if observed.startswith("observed "):
                observed = observed[len("observed "):]
            if ", pre=" in observed:
                observed = observed.split(", pre=", 1)[0]
            print(f"- `{plan['plan']}` -> {plan['verdict']}; invariant: {plan['invariant']}; observed: {observed}")
        print()

    print("\n### Violations and errors\n")
    any_bad = False
    for row in rows:
        for line in row["violations"]:
            print(line)
            any_bad = True
        for line in row["errors"]:
            print(line)
            any_bad = True
    if not any_bad:
        print("(none)")

    print("\n### Hang localisation\n")
    for row in rows:
        if row["hang_count"]:
            print(
                f"- {row['level']} rep {row['rep']}: survivor_start={row.get('survivor_starts',0)} "
                f"survivor_done={row.get('survivor_dones',0)} (missing={row['hang_count']}); "
                f"stderr: {row['stderr_tail']}"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
