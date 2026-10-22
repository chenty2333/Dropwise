"""Cross-Implementation Test on asyncpg.

Evaluates whether the stage definitions, classifier rules, and successor probes
transfer to asyncpg 0.32.0.

Records:
1. Reused stages: enter_wait, body_fetch, commit_wait, sp_enter, sp_release.
2. Reinterpretations needed: asyncpg has no libpq transaction_status or Cython
   wait generators (WAIT_W/WAIT_R); I/O is event-driven via asyncio.Protocol.
3. Private field dependencies: conn.is_in_transaction() vs conn._top_xact.
4. Outcome divergence: when cancelled during commit, asyncpg raises CancelledError
   or TransactionError; connection recycling contract differs from psycopg.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import sys
import time
from typing import Any

import asyncpg

from fragmentation.fragment_proxy import FragmentingProxy
from stage_experiment.harness import StageSteppingTask, TrialResult
from stage_experiment.stage_definitions import Stage, StageClassifier


def asyncpg_url(host: str, port: int, user: str, password: str, dbname: str) -> str:
    return f"postgresql://{user}:{password}@{host}:{port}/{dbname}"


async def execute_asyncpg_trial(
    scenario_runner: Any,
    *,
    mode: str,
    target_cut: Any,
    url: str,
    table_name: str,
    observer_conn: asyncpg.Connection,
) -> tuple[TrialResult, list[Any]]:
    started = time.monotonic()
    context_holder: dict[str, Any] = {
        "method_context": None,
        "conn": None,
        "table": table_name,
        "url": url,
    }

    # Clean table state before trial
    await asyncio.wait_for(
        observer_conn.execute(f"DELETE FROM {table_name}"),
        timeout=3.0,
    )

    task = StageSteppingTask(
        scenario_runner(context_holder),
        mode=mode,
        target_cut=target_cut,
        context_holder=context_holder,
    )

    outcome = "UNKNOWN"
    exc_type = None
    try:
        await asyncio.wait_for(task, timeout=8.0)
        outcome = "COMPLETED"
    except asyncio.CancelledError:
        outcome = "CANCELLED"
        exc_type = "CancelledError"
    except asyncio.TimeoutError:
        outcome = "TIMEOUT"
        exc_type = "TimeoutError"
        task.cancel()
        with contextlib.suppress(BaseException):
            await task
    except BaseException as be:
        outcome = "EXCEPTION"
        exc_type = type(be).__name__

    conn: asyncpg.Connection | None = context_holder.get("conn")
    driver_closed = conn.is_closed() if conn else True

    # Successor probe on the tested handle
    successor_ok = False
    if conn and not driver_closed:
        try:
            res = await asyncio.wait_for(conn.fetchval("SELECT 1"), timeout=2.0)
            successor_ok = (res == 1)
        except Exception:
            successor_ok = False
    elif conn and driver_closed:
        try:
            await asyncio.wait_for(conn.fetchval("SELECT 1"), timeout=2.0)
            successor_ok = False
        except asyncpg.InterfaceError:
            successor_ok = True  # cleanly rejected on closed connection

    # Observer check
    row_cnt = await asyncio.wait_for(
        observer_conn.fetchval(f"SELECT count(*) FROM {table_name}"),
        timeout=2.0,
    )
    observer_rows = row_cnt or 0

    if outcome == "CANCELLED":
        if driver_closed:
            verdict = "FAIL_CLOSED"
        elif observer_rows == 0:
            verdict = "PRE"
        else:
            verdict = "POST"
    elif outcome == "COMPLETED":
        verdict = "POST"
    else:
        verdict = "EXCEPTION"

    if conn and not conn.is_closed():
        with contextlib.suppress(BaseException):
            await asyncio.wait_for(conn.close(), timeout=2.0)

    duration = time.monotonic() - started
    result = TrialResult(
        method=mode,
        target_spec=target_cut,
        realized_yield=task.cut_yield,
        realized_stage=task.cut_stage,
        outcome=outcome,
        exc_type=exc_type,
        driver_closed=driver_closed,
        conn_tx_status=1 if (conn and not driver_closed and conn.is_in_transaction()) else 0,
        successor_ok=successor_ok,
        observer_rows=observer_rows,
        verdict=verdict,
        duration_s=duration,
    )
    return result, task.trace


async def run_scenario_short_tx(holder: dict[str, Any]) -> None:
    conn = holder["conn"]
    table = holder["table"]

    tx = conn.transaction()
    holder["method_context"] = "enter"
    await tx.__aenter__()

    holder["method_context"] = "body"
    await conn.execute(f"INSERT INTO {table} VALUES (1, 'val1')")

    holder["method_context"] = "commit"
    await tx.__aexit__(None, None, None)

    holder["method_context"] = None


async def run_scenario_nested_tx(holder: dict[str, Any]) -> None:
    conn = holder["conn"]
    table = holder["table"]

    tx_outer = conn.transaction()
    holder["method_context"] = "enter"
    await tx_outer.__aenter__()

    holder["method_context"] = "body"
    await conn.execute(f"INSERT INTO {table} VALUES (1, 'outer')")

    tx_inner = conn.transaction()
    holder["method_context"] = "sp_enter"
    await tx_inner.__aenter__()

    holder["method_context"] = "body"
    await conn.execute(f"INSERT INTO {table} VALUES (2, 'inner')")

    holder["method_context"] = "sp_release"
    await tx_inner.__aexit__(None, None, None)

    holder["method_context"] = "commit"
    await tx_outer.__aexit__(None, None, None)

    holder["method_context"] = None


async def run_scenario_fragmented_resp(holder: dict[str, Any]) -> None:
    conn = holder["conn"]
    table = holder["table"]

    tx = conn.transaction()
    holder["method_context"] = "enter"
    await tx.__aenter__()

    holder["method_context"] = "body"
    await conn.fetch("SELECT repeat('a', 32768)")

    holder["method_context"] = "commit"
    await tx.__aexit__(None, None, None)

    holder["method_context"] = None


def make_asyncpg_runner(scenario_fn: Any, url: str):
    async def runner(holder: dict[str, Any]) -> None:
        conn = await asyncpg.connect(url)
        holder["conn"] = conn
        try:
            await scenario_fn(holder)
        finally:
            if not conn.is_closed():
                with contextlib.suppress(BaseException):
                    await conn.close()
    return runner


async def run_asyncpg_comparison(
    scenario_name: str,
    scenario_fn: Any,
    *,
    url: str,
    observer_conn: asyncpg.Connection,
    table_name: str,
) -> dict[str, Any]:
    print(f"\n=======================================================", flush=True)
    print(f"asyncpg Scenario: {scenario_name}", flush=True)
    print(f"=======================================================", flush=True)

    runner = make_asyncpg_runner(scenario_fn, url)

    # 1. Uncancelled calibration run
    uncancelled_res, trace = await execute_asyncpg_trial(
        runner,
        mode="count",
        target_cut=None,
        url=url,
        table_name=table_name,
        observer_conn=observer_conn,
    )
    K = uncancelled_res.realized_yield or len(trace)
    distinct_stages: list[Stage] = []
    for step, stg, _, _, _ in trace:
        if stg != Stage.UNKNOWN and stg not in distinct_stages:
            distinct_stages.append(stg)

    print(f"Normal path completed: K={K} yields, distinct stages={len(distinct_stages)}: {[s.value for s in distinct_stages]}", flush=True)

    # 2. Method A: Yield-Index Replay (1..K)
    print(f"\n--- Method A: Yield-Index Replay (1..{K}) ---", flush=True)
    t_start_a = time.monotonic()
    method_a_results: list[TrialResult] = []
    for k in range(1, K + 1):
        res, _ = await execute_asyncpg_trial(
            runner,
            mode="yield",
            target_cut=k,
            url=url,
            table_name=table_name,
            observer_conn=observer_conn,
        )
        method_a_results.append(res)
    time_a = time.monotonic() - t_start_a
    print(f"Method A completed {K} trials in {time_a:.2f}s", flush=True)

    # 3. Method B: Stage-Targeted Cancellation
    print(f"\n--- Method B: Stage-Targeted Cancellation ({len(distinct_stages)} stages) ---", flush=True)
    t_start_b = time.monotonic()
    method_b_results: list[TrialResult] = []
    for stg in distinct_stages:
        res, _ = await execute_asyncpg_trial(
            runner,
            mode="stage",
            target_cut=stg,
            url=url,
            table_name=table_name,
            observer_conn=observer_conn,
        )
        method_b_results.append(res)
    time_b = time.monotonic() - t_start_b
    print(f"Method B completed {len(distinct_stages)} trials in {time_b:.2f}s", flush=True)

    a_verdicts = [r.verdict for r in method_a_results]
    b_verdicts = [r.verdict for r in method_b_results]
    a_unique = set(a_verdicts)
    b_unique = set(b_verdicts)

    seen_pairs: set[tuple[Stage | None, str]] = set()
    duplicates_a = 0
    for r in method_a_results:
        pair = (r.realized_stage, r.verdict)
        if pair in seen_pairs:
            duplicates_a += 1
        else:
            seen_pairs.add(pair)

    omissions_b = a_unique - b_unique

    print(f"Comparison Summary for asyncpg {scenario_name}:", flush=True)
    print(f"  Method A: {len(method_a_results)} trials, {time_a:.2f}s, unique verdicts={a_unique}, duplicate trials={duplicates_a}", flush=True)
    print(f"  Method B: {len(method_b_results)} trials, {time_b:.2f}s, unique verdicts={b_unique}, omissions={omissions_b}", flush=True)

    return {
        "scenario": scenario_name,
        "K": K,
        "stages": [s.value for s in distinct_stages],
        "method_a": {
            "trials": len(method_a_results),
            "time_s": time_a,
            "verdicts": a_verdicts,
            "unique_verdicts": list(a_unique),
            "duplicates": duplicates_a,
            "details": [{"k": r.target_spec, "stage": r.realized_stage.value if r.realized_stage else None, "verdict": r.verdict} for r in method_a_results],
        },
        "method_b": {
            "trials": len(method_b_results),
            "time_s": time_b,
            "verdicts": b_verdicts,
            "unique_verdicts": list(b_unique),
            "omissions": list(omissions_b),
            "details": [{"target_stage": r.target_spec.value if isinstance(r.target_spec, Stage) else r.target_spec, "realized_yield": r.realized_yield, "verdict": r.verdict} for r in method_b_results],
        },
    }


async def main() -> int:
    host = "127.0.0.1"
    port = 32919
    user = "postgres"
    password = "dw"
    dbname = "dw"

    direct_url = asyncpg_url(host, port, user, password, dbname)
    table_name = f"test_asyncpg_stage_{os.getpid()}"

    observer_conn = await asyncpg.connect(direct_url)
    await observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}")
    await observer_conn.execute(f"CREATE TABLE {table_name} (id int primary key, payload text)")

    overall_results: list[dict[str, Any]] = []

    try:
        # Proxy with mild delay
        proxy = FragmentingProxy(host, port, delay=0.002, mode="off")
        proxy_port = await proxy.start()
        proxy_url = asyncpg_url(host, proxy_port, user, password, dbname)

        # 1. Short Tx
        res_short = await run_asyncpg_comparison(
            "Short_Transaction",
            run_scenario_short_tx,
            url=proxy_url,
            observer_conn=observer_conn,
            table_name=table_name,
        )
        overall_results.append(res_short)

        # 2. Nested Tx (Savepoint)
        res_nested = await run_asyncpg_comparison(
            "Nested_Transaction_Savepoint",
            run_scenario_nested_tx,
            url=proxy_url,
            observer_conn=observer_conn,
            table_name=table_name,
        )
        overall_results.append(res_nested)

        with contextlib.suppress(BaseException):
            await asyncio.wait_for(proxy.close(), timeout=2.0)

        # 3. Response fragmentation
        proxy_frag = FragmentingProxy(host, port, delay=0.002, mode="payload", pause=0.005)
        frag_port = await proxy_frag.start()
        frag_url = asyncpg_url(host, frag_port, user, password, dbname)

        res_frag = await run_asyncpg_comparison(
            "Fragmented_Response",
            run_scenario_fragmented_resp,
            url=frag_url,
            observer_conn=observer_conn,
            table_name=table_name,
        )
        overall_results.append(res_frag)

        with contextlib.suppress(BaseException):
            await asyncio.wait_for(proxy_frag.close(), timeout=2.0)

        out_path = "/home/ava/Desktop/Dropwise/prospective/phase4-exploration/python/pg-mysql/stage_experiment/asyncpg_results.json"
        with open(out_path, "w") as f:
            json.dump(overall_results, f, indent=2)
        print(f"\nAll asyncpg cross-implementation experiments completed! Results saved to {out_path}", flush=True)

        return 0

    finally:
        with contextlib.suppress(BaseException):
            await observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}")
            await observer_conn.close()


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
