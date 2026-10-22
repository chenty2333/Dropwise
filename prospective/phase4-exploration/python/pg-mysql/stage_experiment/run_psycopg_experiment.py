"""Run the Stage-Informed vs Yield-Replay Experiment on Psycopg.

Evaluates:
- 5 scenarios: Short Tx, Nested Tx, Fragmented Response, Partial Send, Double Cancel
- 3 network/workload conditions:
    C1 (Baseline): 2048 B chunks, 5ms delay
    C2 (High Fragmentation): 256 B chunks, 5ms delay, 16KB payload (Pending explosion)
    C3 (Backpressure): 2MB payload, 32KB client hold, release on cut
- Compares:
    Method A: Step-by-step Yield Replay (1..K)
    Method B: Stage-Targeted Cancellation (1 trial per semantic stage)
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import sys
import time
from typing import Any

import psycopg

from fragmentation.fragment_proxy import FragmentingProxy
from stage_experiment.harness import execute_trial, TrialResult
from stage_experiment.stage_definitions import Stage


class TrackerGen:
    def __init__(self, gen: Any, holder: dict[str, Any]) -> None:
        self._gen = gen
        self._holder = holder
        self.last_wait: int | None = None
        holder["tracker"] = self

    def __iter__(self) -> TrackerGen:
        return self

    def __next__(self) -> Any:
        s = next(self._gen)
        self.last_wait = s
        return s

    def send(self, val: Any) -> Any:
        s = self._gen.send(val)
        self.last_wait = s
        return s

    def throw(self, *args: Any) -> Any:
        return self._gen.throw(*args)

    def close(self) -> None:
        self._gen.close()


def instrument_conn(conn: psycopg.AsyncConnection, holder: dict[str, Any]) -> None:
    holder["conn"] = conn
    orig_wait = conn.wait

    async def hooked_wait(gen: Any, interval: float = 0.1, timeout: float | None = None) -> Any:
        tracker = TrackerGen(gen, holder)
        return await orig_wait(tracker, interval=interval, timeout=timeout)

    conn.wait = hooked_wait  # type: ignore[method-assign]


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
    cur = await conn.execute("SELECT repeat('a', 32768)")
    await cur.fetchall()

    holder["method_context"] = "commit"
    await tx.__aexit__(None, None, None)

    holder["method_context"] = None


async def run_scenario_partial_send(holder: dict[str, Any]) -> None:
    conn = holder["conn"]
    table = holder["table"]
    payload = "x" * 8_000_000

    tx = conn.transaction()
    holder["method_context"] = "enter"
    await tx.__aenter__()

    holder["method_context"] = "body"
    await conn.execute(f"INSERT INTO {table} VALUES (1, %s)", (payload,))

    holder["method_context"] = "commit"
    await tx.__aexit__(None, None, None)

    holder["method_context"] = None


async def run_scenario_double_cancel(holder: dict[str, Any]) -> None:
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


def make_driver_runner(scenario_fn: Any, dsn: str):
    async def runner(holder: dict[str, Any]) -> None:
        conn = await psycopg.AsyncConnection.connect(dsn, autocommit=True)
        instrument_conn(conn, holder)
        try:
            await scenario_fn(holder)
        finally:
            if not conn.closed:
                with contextlib.suppress(BaseException):
                    await conn.close()
    return runner


async def run_comparison_suite(
    scenario_name: str,
    scenario_fn: Any,
    *,
    proxy_dsn: str,
    observer_conn: psycopg.AsyncConnection,
    table_name: str,
    proxy: Any = None,
    is_partial_send: bool = False,
    second_cut_stage: Stage | None = None,
) -> dict[str, Any]:
    print(f"\n=======================================================")
    print(f"Scenario: {scenario_name}")
    print(f"=======================================================")

    runner = make_driver_runner(scenario_fn, proxy_dsn)

    # 1. Uncancelled calibration run to record normal-path trace and K
    if is_partial_send and proxy:
        proxy.arm_hold(32768)
        asyncio.get_running_loop().call_later(0.1, proxy.release_hold)

    uncancelled_res, trace = await execute_trial(
        runner,
        mode="count",
        target_cut=None,
        dsn=proxy_dsn,
        table_name=table_name,
        observer_conn=observer_conn,
    )
    K = uncancelled_res.realized_yield or len(trace)
    distinct_stages: list[Stage] = []
    for step, stg, _, _, _ in trace:
        if stg != Stage.UNKNOWN and stg not in distinct_stages:
            distinct_stages.append(stg)

    print(f"Normal path completed: K={K} yields, distinct stages={len(distinct_stages)}: {[s.value for s in distinct_stages]}")

    # 2. Method A: Yield-Index Replay (1..K)
    print(f"\n--- Method A: Yield-Index Replay (1..{K}) ---")
    t_start_a = time.monotonic()
    method_a_results: list[TrialResult] = []
    for k in range(1, K + 1):
        hook = None
        if is_partial_send and proxy:
            proxy.arm_hold(32768)
            hook = proxy.release_hold

        res, _ = await execute_trial(
            runner,
            mode="yield",
            target_cut=k,
            dsn=proxy_dsn,
            table_name=table_name,
            on_cut_hook=hook,
            second_cut_stage=second_cut_stage,
            observer_conn=observer_conn,
        )
        method_a_results.append(res)
    time_a = time.monotonic() - t_start_a
    print(f"Method A completed {K} trials in {time_a:.2f}s")

    # 3. Method B: Stage-Targeted Cancellation (1 trial per distinct stage)
    print(f"\n--- Method B: Stage-Targeted Cancellation ({len(distinct_stages)} stages) ---")
    t_start_b = time.monotonic()
    method_b_results: list[TrialResult] = []
    for stg in distinct_stages:
        hook = None
        # Backpressure hold is an environmental precondition specific to BODY_SEND.
        # Arming it for subsequent stages would block them from ever being reached.
        if is_partial_send and proxy:
            if stg == Stage.BODY_SEND:
                proxy.arm_hold(32768)
                hook = proxy.release_hold
            else:
                proxy.release_hold()

        res, _ = await execute_trial(
            runner,
            mode="stage",
            target_cut=stg,
            dsn=proxy_dsn,
            table_name=table_name,
            on_cut_hook=hook,
            second_cut_stage=second_cut_stage,
            observer_conn=observer_conn,
        )
        method_b_results.append(res)
    time_b = time.monotonic() - t_start_b
    print(f"Method B completed {len(distinct_stages)} trials in {time_b:.2f}s")

    # Analyze comparison
    a_verdicts = [r.verdict for r in method_a_results]
    b_verdicts = [r.verdict for r in method_b_results]
    a_unique_verdicts = set(a_verdicts)
    b_unique_verdicts = set(b_verdicts)

    # Duplicates in Method A: trials that map to already seen (stage, verdict)
    seen_pairs: set[tuple[Stage | None, str]] = set()
    duplicates_a = 0
    for r in method_a_results:
        pair = (r.realized_stage, r.verdict)
        if pair in seen_pairs:
            duplicates_a += 1
        else:
            seen_pairs.add(pair)

    # Omissions in Method B: did Method A discover a verdict that B missed?
    omissions_b = a_unique_verdicts - b_unique_verdicts

    print(f"Comparison Summary for {scenario_name}:")
    print(f"  Method A: {len(method_a_results)} trials, {time_a:.2f}s, unique verdicts={a_unique_verdicts}, duplicate trials={duplicates_a}")
    print(f"  Method B: {len(method_b_results)} trials, {time_b:.2f}s, unique verdicts={b_unique_verdicts}, omissions={omissions_b}")

    return {
        "scenario": scenario_name,
        "K": K,
        "stages": [s.value for s in distinct_stages],
        "method_a": {
            "trials": len(method_a_results),
            "time_s": time_a,
            "verdicts": a_verdicts,
            "unique_verdicts": list(a_unique_verdicts),
            "duplicates": duplicates_a,
            "details": [{"k": r.target_spec, "stage": r.realized_stage.value if r.realized_stage else None, "verdict": r.verdict} for r in method_a_results],
        },
        "method_b": {
            "trials": len(method_b_results),
            "time_s": time_b,
            "verdicts": b_verdicts,
            "unique_verdicts": list(b_unique_verdicts),
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

    direct_dsn = f"host={host} port={port} user={user} password={password} dbname={dbname} sslmode=disable"
    table_name = f"test_stage_exp_{os.getpid()}"

    observer_conn = await psycopg.AsyncConnection.connect(direct_dsn, autocommit=True)
    await observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}")
    await observer_conn.execute(f"CREATE TABLE {table_name} (id int primary key, payload text)")

    overall_results: list[dict[str, Any]] = []

    try:
        # =========================================================================
        # CONDITION 1: Baseline (Direct / Low Delay)
        # =========================================================================
        print("\n=======================================================")
        print("CONDITION 1: Baseline (Low Delay Proxy, 2048 B chunks)")
        print("=======================================================")
        proxy_c1 = FragmentingProxy(host, port, delay=0.002, mode="off")
        proxy_port_c1 = await proxy_c1.start()
        proxy_dsn_c1 = f"host={host} port={proxy_port_c1} user={user} password={password} dbname={dbname} sslmode=disable"

        res_short = await run_comparison_suite(
            "C1_Short_Transaction",
            run_scenario_short_tx,
            proxy_dsn=proxy_dsn_c1,
            observer_conn=observer_conn,
            table_name=table_name,
            proxy=proxy_c1,
        )
        overall_results.append(res_short)

        res_nested = await run_comparison_suite(
            "C1_Nested_Transaction_Savepoint",
            run_scenario_nested_tx,
            proxy_dsn=proxy_dsn_c1,
            observer_conn=observer_conn,
            table_name=table_name,
            proxy=proxy_c1,
        )
        overall_results.append(res_nested)

        with contextlib.suppress(BaseException):
            await asyncio.wait_for(proxy_c1.close(), timeout=2.0)

        # =========================================================================
        # CONDITION 2: High Fragmentation (Response Chunks with delay)
        # =========================================================================
        print("\n=======================================================")
        print("CONDITION 2: High Response Fragmentation (Pending Explosion)")
        print("=======================================================")
        proxy_c2 = FragmentingProxy(host, port, delay=0.002, mode="payload", pause=0.005)
        proxy_port_c2 = await proxy_c2.start()
        proxy_dsn_c2 = f"host={host} port={proxy_port_c2} user={user} password={password} dbname={dbname} sslmode=disable"

        res_frag = await run_comparison_suite(
            "C2_Fragmented_Response",
            run_scenario_fragmented_resp,
            proxy_dsn=proxy_dsn_c2,
            observer_conn=observer_conn,
            table_name=table_name,
            proxy=proxy_c2,
        )
        overall_results.append(res_frag)

        with contextlib.suppress(BaseException):
            await asyncio.wait_for(proxy_c2.close(), timeout=2.0)

        # =========================================================================
        # CONDITION 3: Backpressure & Partial Send
        # =========================================================================
        print("\n=======================================================")
        print("CONDITION 3: Large Send & Backpressure Interleaving")
        print("=======================================================")
        proxy_c3 = FragmentingProxy(host, port, delay=0.0, mode="off")
        proxy_port_c3 = await proxy_c3.start(rcvbuf=16384)
        proxy_dsn_c3 = f"host={host} port={proxy_port_c3} user={user} password={password} dbname={dbname} sslmode=disable"

        res_psend = await run_comparison_suite(
            "C3_Partial_Send_Backpressure",
            run_scenario_partial_send,
            proxy_dsn=proxy_dsn_c3,
            observer_conn=observer_conn,
            table_name=table_name,
            proxy=proxy_c3,
            is_partial_send=True,
        )
        overall_results.append(res_psend)

        res_double = await run_comparison_suite(
            "C3_Double_Cancel_Entry_Recovery",
            run_scenario_double_cancel,
            proxy_dsn=proxy_dsn_c3,
            observer_conn=observer_conn,
            table_name=table_name,
            proxy=proxy_c3,
            second_cut_stage=Stage.RECOVERY_DRAIN,
        )
        overall_results.append(res_double)

        with contextlib.suppress(BaseException):
            await asyncio.wait_for(proxy_c3.close(), timeout=2.0)

        # Save results to JSON
        out_path = "/home/ava/Desktop/Dropwise/prospective/phase4-exploration/python/pg-mysql/stage_experiment/psycopg_results.json"
        with open(out_path, "w") as f:
            json.dump(overall_results, f, indent=2)
        print(f"\nAll psycopg comparison experiments completed! Results saved to {out_path}", flush=True)

        return 0

    finally:
        with contextlib.suppress(BaseException):
            await asyncio.wait_for(observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}"), timeout=2.0)
            await asyncio.wait_for(observer_conn.close(), timeout=2.0)


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
