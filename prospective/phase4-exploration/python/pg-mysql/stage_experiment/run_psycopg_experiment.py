"""Run the Stage-Informed vs Yield-Replay Experiment on Psycopg.

Five scenarios: short/nested transactions, fragmented responses, partial send,
and two real cancellations during query recovery. Calibration and both methods
share a 2 ms chunk-delay baseline, or one 5 ms pause inside each v3 message,
or an 8 MB send with a 32 KiB proxy hold released after 100 ms independently of
cancellation. Method A enumerates target-operation suspensions; Method B picks
the first suspension of each (operation identity, observed stage) pair.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import sys
from typing import Any

import psycopg

from fragmentation.fragment_proxy import FragmentingProxy
from stage_experiment.harness import compare
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


def instrument_conn(conn, holder):
    holder["conn"] = conn
    orig_wait = conn.wait
    orig_cancel = conn._try_cancel

    async def hooked_cancel(*, timeout=5.0):
        holder["method_context"] = "recovery"
        holder["recovery_entered"] = True
        return await orig_cancel(timeout=timeout)

    async def hooked_wait(gen, interval=0.1, timeout=None):
        context = holder.get("method_context")
        holder["recovery_entered"] = False
        tracker = TrackerGen(gen, holder)
        try:
            return await orig_wait(tracker, interval=interval, timeout=timeout)
        except asyncio.CancelledError:
            # This flag requires an actual second Task.cancel(), and delivery
            # while the driver's recovery path is unwinding. No synthetic raise.
            task = asyncio.current_task()
            if holder["recovery_entered"] and task.cancelling() >= 2:
                holder["second_delivered"] = True
            raise
        finally:
            holder["method_context"] = context
            holder["tracker"] = None

    conn.wait = hooked_wait
    conn._try_cancel = hooked_cancel


def phase(holder, context, operation):
    holder["method_context"] = context
    holder["operation"] = operation


@contextlib.asynccontextmanager
async def transaction(holder, *, nested=False):
    tx = holder["conn"].transaction()
    scope = "inner" if nested else "outer"
    phase(holder, "sp_enter" if nested else "enter", scope + ".enter")
    await tx.__aenter__()
    try:
        yield
    except BaseException:
        phase(holder, "rollback", scope + ".rollback")
        if not await tx.__aexit__(*sys.exc_info()):
            raise
    else:
        phase(holder, "sp_release" if nested else "commit", scope + ".commit")
        await tx.__aexit__(None, None, None)
    finally:
        phase(holder, None, None)


async def run_scenario_short_tx(holder):
    async with transaction(holder):
        phase(holder, "body", "outer.insert")
        await holder["conn"].execute(
            f"INSERT INTO {holder['table']} VALUES (1, 'val1')"
        )


async def run_scenario_nested_tx(holder):
    holder["expected_rows"] = [1, 2]
    async with transaction(holder):
        phase(holder, "body", "outer.insert")
        await holder["conn"].execute(
            f"INSERT INTO {holder['table']} VALUES (1, 'outer')"
        )
        async with transaction(holder, nested=True):
            phase(holder, "body", "inner.insert")
            await holder["conn"].execute(
                f"INSERT INTO {holder['table']} VALUES (2, 'inner')"
            )


async def run_scenario_fragmented_resp(holder):
    holder["expected_rows"] = []
    async with transaction(holder):
        phase(holder, "body", "outer.fetch")
        await holder["conn"].execute("SELECT repeat('a', 32768)")


async def run_scenario_partial_send(holder):
    async with transaction(holder):
        phase(holder, "body", "outer.insert")
        await holder["conn"].execute(
            f"INSERT INTO {holder['table']} VALUES (1, %s)", ("x" * 8_000_000,)
        )


async def run_scenario_double_cancel(holder):
    holder["expected_rows"] = []
    # Autocommit removes transaction-context cleanup as a confounder: both
    # requests must hit query cancellation and the driver's recovery itself.
    phase(holder, "body", "query.sleep")
    await holder["conn"].execute("SELECT pg_sleep(2)")
    phase(holder, None, None)


def make_driver_runner(scenario_fn, dsn, *, proxy=None, is_partial_send=False):
    async def prepare(holder):
        conn = await psycopg.AsyncConnection.connect(dsn, autocommit=True)
        instrument_conn(conn, holder)
        if proxy is not None:
            proxy.reset_stats()
        if is_partial_send:
            # Identical hold/release schedule for calibration and both methods.
            # Never release because of a cut or because B targets a later stage.
            proxy.arm_hold(32768)
            timer = asyncio.get_running_loop().call_later(0.1, proxy.release_hold)

            def cleanup():
                timer.cancel()
                proxy.release_hold()

            holder["cleanup"] = cleanup

    async def runner(holder):
        await scenario_fn(holder)

    runner.prepare = prepare
    return runner


async def run_comparison_suite(
    scenario_name,
    scenario_fn,
    *,
    proxy_dsn,
    observer_conn,
    table_name,
    proxy=None,
    is_partial_send=False,
    second_cut_stage=None,
):
    runner = make_driver_runner(
        scenario_fn, proxy_dsn, proxy=proxy, is_partial_send=is_partial_send
    )
    result = await compare(
        runner,
        dsn=proxy_dsn,
        table_name=table_name,
        observer_conn=observer_conn,
        second_cut_stage=second_cut_stage,
    )
    result["scenario"] = scenario_name
    print(
        f"{scenario_name}: valid={result['valid']} A={result['method_a']['trials']} "
        f"B={result['method_b']['trials']} missing={len(result['missing_observations'])} "
        f"unrealized={result['unrealized_trials']}",
        flush=True,
    )
    return result


async def main() -> int:
    host = "127.0.0.1"
    port = int(os.environ.get("PGPORT", "32919"))
    user = "postgres"
    password = "dw"
    dbname = "dw"

    direct_dsn = f"host={host} port={port} user={user} password={password} dbname={dbname} sslmode=disable"
    table_name = f"test_stage_exp_{os.getpid()}"

    observer_conn = await psycopg.AsyncConnection.connect(direct_dsn, autocommit=True)
    await observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}")
    await observer_conn.execute(
        f"CREATE TABLE {table_name} (id int primary key, payload text)"
    )

    overall_results: list[dict[str, Any]] = []

    try:
        # =========================================================================
        # CONDITION 1: Baseline (Direct / Low Delay)
        # =========================================================================
        print("\n=======================================================")
        print("CONDITION 1: Baseline (2 ms chunk delay)")
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
        print("CONDITION 2: Finite response splits (5 ms pause)")
        print("=======================================================")
        proxy_c2 = FragmentingProxy(
            host, port, delay=0.002, mode="payload", pause=0.005
        )
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
            "C3_Double_Cancel_Query_Recovery",
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
        out_path = os.path.join(os.path.dirname(__file__), "psycopg_results.json")
        with open(out_path, "w") as f:
            json.dump(overall_results, f, indent=2)
        print(
            f"\nPsycopg comparisons finished; inspect validity and omissions. Results saved to {out_path}",
            flush=True,
        )

        return 0 if all(r["valid"] for r in overall_results) else 1

    finally:
        with contextlib.suppress(BaseException):
            await asyncio.wait_for(
                observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}"), timeout=2.0
            )
            await asyncio.wait_for(observer_conn.close(), timeout=2.0)


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
