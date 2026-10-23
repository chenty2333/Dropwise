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
from typing import Any

import asyncpg

from fragmentation.fragment_proxy import FragmentingProxy
from stage_experiment.harness import compare
from stage_experiment.run_psycopg_experiment import (
    run_scenario_short_tx,
    run_scenario_nested_tx,
    run_scenario_fragmented_resp,
)


def asyncpg_url(host, port, user, password, dbname):
    return f"postgresql://{user}:{password}@{host}:{port}/{dbname}"


def make_asyncpg_runner(scenario_fn, url):
    async def prepare(holder):
        # The fragmentation proxy is a plaintext v3 fixture, not a TLS proxy.
        holder["conn"] = await asyncpg.connect(url, ssl=False)
        holder["driver"] = "asyncpg"

    async def runner(holder):
        await scenario_fn(holder)

    runner.prepare = prepare
    return runner


async def run_asyncpg_comparison(
    scenario_name, scenario_fn, *, url, observer_conn, table_name
):
    result = await compare(
        make_asyncpg_runner(scenario_fn, url),
        dsn=url,
        table_name=table_name,
        observer_conn=observer_conn,
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

    direct_url = asyncpg_url(host, port, user, password, dbname)
    table_name = f"test_asyncpg_stage_{os.getpid()}"

    observer_conn = await asyncpg.connect(direct_url, ssl=False)
    await observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}")
    await observer_conn.execute(
        f"CREATE TABLE {table_name} (id int primary key, payload text)"
    )

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
        proxy_frag = FragmentingProxy(
            host, port, delay=0.002, mode="payload", pause=0.005
        )
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

        out_path = os.path.join(os.path.dirname(__file__), "asyncpg_results.json")
        with open(out_path, "w") as f:
            json.dump(overall_results, f, indent=2)
        print(
            f"\nAll asyncpg cross-implementation experiments completed! Results saved to {out_path}",
            flush=True,
        )

        return 0 if all(r["valid"] for r in overall_results) else 1

    finally:
        with contextlib.suppress(BaseException):
            await observer_conn.execute(f"DROP TABLE IF EXISTS {table_name}")
            await observer_conn.close()


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
