"""Supplementary coverage diagnostic: cancel *after* the server has the request.

The engine ladder cancels at the target's k-th suspension.  For curl-cffi the
only Python suspension is the outer ``await`` around libcurl, and the engine's
cut at that suspension lands before libcurl writes the request (the M1
observation has ``target_logged: False``).  This diagnostic covers the missing
regime deterministically: start the target request, poll the *independent
observer* until the server has logged that request, then ``Task.cancel()`` the
still-running target.  The survivor and the final observer read are the same
as in M1.

This is a coverage probe, not an engine plan: its result is reported as a
diagnostic in RESULTS-FRAGMENT-ROUND3-TARGETS.md and is not counted as a
pending#k trial.  One DelayProxy per probe instance, same 0.05 s/chunk pending
mechanism as the ladder.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import time

from .support import DelayProxy, unique


def make_scenario(target: str, url: str):
    if target == "aiohttp":
        from .aiohttp_case import AiohttpScenario

        return AiohttpScenario(url)
    if target == "curl-cffi":
        from .curl_cffi_case import CurlCffiScenario

        return CurlCffiScenario(url)
    raise SystemExit(f"unknown target {target}")


async def wait_logged(sc, target_req: str, deadline: float = 1.5):
    """Poll the direct observer until the target request is in the server log."""
    started = time.monotonic()
    while time.monotonic() - started < deadline:
        raw = await sc._probe(  # noqa: SLF001 - diagnostic reuses the probe helper
            sc.observer, f"{sc.server_url}/__observe__?doc={sc.doc}", "poll"
        )
        rows = raw.get("body", {}).get("requests", []) if isinstance(raw, dict) else []
        if any(row.get("req") == target_req for row in rows):
            return round(time.monotonic() - started, 4), len(rows)
        await asyncio.sleep(0.005)
    return None, None


async def one_trial(target: str, url: str, rep: int) -> dict:
    sc = make_scenario(target, url)
    await sc.setup()
    try:
        task = asyncio.create_task(sc.target())
        seen_after, rows_before = await wait_logged(sc, sc.target_req)
        seen = seen_after is not None
        if seen:
            task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        outcome = (
            "cancelled" if task.cancelled() else ("completed" if task.done() else "running")
        )
        survivor = await sc.survivor()
        observer = await sc.observe()
        target_state = (
            survivor.get("target_logged") if isinstance(survivor, dict) else None
        )
        return {
            "rep": rep,
            "target_req_seen_before_cancel": seen,
            "target_seen_after_s": seen_after,
            "observer_rows_at_cancel": rows_before,
            "cancel_delivered": task.cancelled(),
            "task_outcome": outcome,
            "target_logged_final": target_state,
            "survivor": survivor,
            "observer": observer,
        }
    finally:
        await sc.close()


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("aiohttp", "curl-cffi"), required=True)
    parser.add_argument("--url", required=True)
    parser.add_argument("--reps", type=int, default=3)
    args = parser.parse_args()
    if args.target == "aiohttp":
        import aiohttp as client_pkg

        version = f"aiohttp={client_pkg.__version__}"
    else:
        import curl_cffi as client_pkg

        version = f"curl_cffi={client_pkg.__version__} curl={client_pkg.__curl_version__}"
    print(
        f"probe=mid-flight-cancel target={args.target} {version} url={args.url} "
        f"reps={args.reps} trigger=observer-confirmed-request-logged; "
        "diagnostic, not an engine plan",
        flush=True,
    )
    trials = []
    for rep in range(1, args.reps + 1):
        result = await one_trial(args.target, args.url, rep)
        trials.append(result)
        print(f"TRIAL {json.dumps(result, sort_keys=True)}", flush=True)
    summary = {
        "trials": len(trials),
        "seen_before_cancel": sum(t["target_req_seen_before_cancel"] for t in trials),
        "target_logged_final": sum(bool(t["target_logged_final"]) for t in trials),
        "survivor_ok": sum(
            bool((t["survivor"] or {}).get("survivor_post_ok"))
            and bool((t["survivor"] or {}).get("readback_ok"))
            for t in trials
        ),
    }
    print(f"PROBE_SUMMARY {json.dumps(summary, sort_keys=True)}", flush=True)


if __name__ == "__main__":
    import contextlib

    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
