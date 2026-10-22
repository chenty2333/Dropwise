"""Minimal reproduction & verification: psycopg cancel during a blocked send.

Verifies:
1. Cancellation during a blocked partial send fails closed.
2. Driver itself closes the connection and rejects subsequent operations;
   the test validator never calls conn.close() to mask driver omissions.
3. Independent observer records:
   - Client-side close timing;
   - Proxy upstream socket close timing (proxy remains alive to distinguish driver
     vs proxy delays);
   - Server-side backend process termination in pg_stat_activity (bounded wait);
   - Server-side lock clearance in pg_locks;
   - Independent table lock acquisition and uncorrupted row state.
4. Robust failure modes and negative controls: unclosed driver, wrong exception,
   or unconfirmed server cleanup causes the validator to fail cleanly without
   being rescued by finally blocks.

Run (from prospective/phase4-exploration/python/pg-mysql):
  PYTHONPATH=".:$REPO/research/python-retro" \
    ~/dropwise-py-work-r3/venv313/bin/python -m fragmentation.min_repro --port 32919 --fix
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import os
import sys
import time

import psycopg
from psycopg import errors as pe

from fragmentation.fragment_proxy import FragmentingProxy, send_q

PAYLOAD = "x" * 8_000_000


async def run_repro(
    host: str = "127.0.0.1",
    port: int = 32919,
    user: str = "postgres",
    password: str = "dw",
    dbname: str = "dw",
    fix: bool = False,
    negative_control: str = "none",
) -> int:
    if fix:
        from fragmentation.partial_send_fix import install

        install()

    table_name = f"test_min_repro_{os.getpid()}_{int(time.time() * 1000) % 1_000_000}"
    observer: psycopg.AsyncConnection | None = None
    proxy: FragmentingProxy | None = None
    conn: psycopg.AsyncConnection | None = None
    exit_code = 0

    try:
        # 1. Independent observer (direct connection, NOT via proxy)
        observer = await asyncio.wait_for(
            psycopg.AsyncConnection.connect(
                f"host={host} port={port} user={user} password={password} "
                f"dbname={dbname} sslmode=disable",
                autocommit=True,
            ),
            timeout=5.0,
        )
        await asyncio.wait_for(
            observer.execute(f"DROP TABLE IF EXISTS {table_name}"),
            timeout=5.0,
        )
        await asyncio.wait_for(
            observer.execute(
                f"CREATE TABLE {table_name} (id int primary key, payload text)"
            ),
            timeout=5.0,
        )

        # 2. Start proxy
        proxy = FragmentingProxy(host, port, delay=0.0, mode="off")
        proxy_port = await asyncio.wait_for(proxy.start(), timeout=5.0)

        # 3. Connect tested client via proxy
        conn = await asyncio.wait_for(
            psycopg.AsyncConnection.connect(
                f"host={host} port={proxy_port} user={user} password={password} "
                f"dbname={dbname} sslmode=disable",
                autocommit=True,
            ),
            timeout=5.0,
        )
        tested_pid = conn.pgconn.backend_pid
        fd = conn.pgconn.socket

        # Verify tested PID is visible server-side
        cur = await asyncio.wait_for(
            observer.execute(
                "SELECT count(*) FROM pg_stat_activity WHERE pid = %s", (tested_pid,)
            ),
            timeout=5.0,
        )
        act_row = await cur.fetchone()
        assert act_row and act_row[0] == 1, (
            "Tested PID must be active in pg_stat_activity"
        )
        print(f"tested connection backend PID: {tested_pid}")

        # 4. Partial send with proxy backpressure hold
        proxy.arm_hold(65536)
        task = asyncio.create_task(
            conn.execute(f"INSERT INTO {table_name} VALUES (1, %s)", (PAYLOAD,))
        )
        await asyncio.wait_for(proxy.hold_reached.wait(), timeout=5.0)
        await asyncio.sleep(0.3)
        print(
            f"before cancel: done={task.done()} send_q={send_q(fd)} "
            f"proxy_bytes={proxy.snapshot()['bytes_c2s']} "
            f"flush={conn.pgconn.flush()} tx={conn.info.transaction_status}",
            flush=True,
        )

        # 5. Cancel task and record outcome
        t_cancel_start = time.monotonic()
        task.cancel()
        proxy.release_hold()

        target_outcome = None
        target_exc: BaseException | None = None
        try:
            await asyncio.wait_for(task, timeout=5.0)
            target_outcome = "COMPLETED"
        except asyncio.CancelledError as exc:
            target_outcome = "CANCELLED"
            target_exc = exc
        except asyncio.TimeoutError as exc:
            target_outcome = "TIMEOUT"
            target_exc = exc
        except BaseException as exc:
            target_outcome = "EXCEPTION"
            target_exc = exc

        duration_to_cancel = time.monotonic() - t_cancel_start

        # Negative control: simulated wrong exception
        if negative_control == "wrong-exception":
            target_outcome = "EXCEPTION"
            target_exc = TypeError("simulated wrong exception (e.g. an integer is required)")

        print(
            f"target task outcome: {target_outcome} "
            f"(took {duration_to_cancel:.3f}s, exc={type(target_exc).__name__ if target_exc else None})",
            flush=True,
        )

        if target_outcome != "CANCELLED":
            print(
                f"FAILURE: target task did not raise CancelledError (got {target_outcome}: {target_exc})",
                flush=True,
            )
            exit_code = 1

        # 6. Check driver's own state BEFORE any manual cleanup
        # The driver MUST have closed the connection itself. The test will not
        # call conn.close() to mask driver omissions.
        is_driver_closed = conn.closed
        if negative_control == "driver-unclosed":
            is_driver_closed = False

        t_client_closed = time.monotonic()
        print(f"driver self-closed: {is_driver_closed}", flush=True)

        if not is_driver_closed:
            print(
                "FAILURE: driver failed to close connection upon partial-send cancellation!",
                flush=True,
            )
            exit_code = 1
        else:
            # Check subsequent operation on closed connection
            try:
                await asyncio.wait_for(conn.execute("SELECT 1"), timeout=2.0)
                print(
                    "FAILURE: driver allowed execute() on closed connection!",
                    flush=True,
                )
                exit_code = 1
            except pe.OperationalError as oe:
                if "closed" not in str(oe).lower():
                    print(
                        f"WARNING: unexpected error message on closed handle: {oe}",
                        flush=True,
                    )
            except BaseException as exc:
                print(
                    f"FAILURE: unexpected exception on closed handle: {type(exc).__name__}: {exc}",
                    flush=True,
                )
                exit_code = 1

        # 7. Record proxy upstream close (DO NOT destroy proxy early)
        t_proxy_upstream = None
        try:
            await asyncio.wait_for(proxy.upstream_closed.wait(), timeout=3.0)
            t_proxy_upstream = proxy.upstream_closed_time or time.monotonic()
            proxy_delay = t_proxy_upstream - t_client_closed
            print(
                f"proxy upstream closed: True (+{proxy_delay:.4f}s after client close)",
                flush=True,
            )
        except asyncio.TimeoutError:
            print(
                "WARNING: proxy upstream connection did not close within 3s",
                flush=True,
            )

        # 8. Bounded wait for server-side backend termination & locks release via observer
        backend_gone = False
        t_backend_gone = None
        if negative_control != "unconfirmed-cleanup":
            deadline = time.monotonic() + 5.0
            while time.monotonic() < deadline:
                cur = await asyncio.wait_for(
                    observer.execute(
                        "SELECT count(*) FROM pg_stat_activity WHERE pid = %s",
                        (tested_pid,),
                    ),
                    timeout=2.0,
                )
                act_row = await cur.fetchone()
                act_cnt = act_row[0] if act_row else 0

                cur = await asyncio.wait_for(
                    observer.execute(
                        "SELECT count(*) FROM pg_locks WHERE pid = %s",
                        (tested_pid,),
                    ),
                    timeout=2.0,
                )
                lock_row = await cur.fetchone()
                lock_cnt = lock_row[0] if lock_row else 0

                if act_cnt == 0 and lock_cnt == 0:
                    backend_gone = True
                    t_backend_gone = time.monotonic()
                    break
                await asyncio.sleep(0.05)

        if not backend_gone:
            print(
                f"FAILURE: backend cleanup UNCONFIRMED for PID {tested_pid} after 5.0s!",
                flush=True,
            )
            return 1

        ref_time = t_proxy_upstream if t_proxy_upstream is not None else t_client_closed
        backend_cleanup_delay = t_backend_gone - ref_time
        print(
            f"server backend gone: True (+{backend_cleanup_delay:.4f}s after upstream close; "
            f"observer confirmed 0 in pg_stat_activity, 0 in pg_locks)",
            flush=True,
        )

        # 9. Independent lock and table integrity check
        async with observer.transaction():
            await asyncio.wait_for(
                observer.execute(
                    f"LOCK TABLE {table_name} IN ACCESS EXCLUSIVE MODE NOWAIT"
                ),
                timeout=2.0,
            )
        cur = await asyncio.wait_for(
            observer.execute(f"SELECT count(*) FROM {table_name}"),
            timeout=2.0,
        )
        row = await cur.fetchone()
        cnt = row[0] if row else -1
        print(
            f"independent lock check: ACCESS EXCLUSIVE acquired; table row count: {cnt} (expected 0)",
            flush=True,
        )
        if cnt != 0:
            print(f"FAILURE: cancelled insert committed {cnt} rows!", flush=True)
            exit_code = 1

        return exit_code

    finally:
        # Bounded teardown in reverse dependency order:
        # 1. Close tested connection if still open (prevents holding table locks)
        if conn is not None and not conn.closed:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(conn.close(), timeout=2.0)

        # 2. Observer cleans up isolated table and closes
        if observer is not None and not observer.closed:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(
                    observer.execute(f"DROP TABLE IF EXISTS {table_name}"),
                    timeout=2.0,
                )
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(observer.close(), timeout=2.0)

        # 3. Proxy shuts down
        if proxy is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(proxy.close(), timeout=2.0)


async def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=32919)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--fix", action="store_true")
    p.add_argument(
        "--negative-control",
        choices=["none", "driver-unclosed", "wrong-exception", "unconfirmed-cleanup"],
        default="none",
    )
    a = p.parse_args()
    return await run_repro(
        host=a.host,
        port=a.port,
        user=a.user,
        password=a.password,
        dbname=a.dbname,
        fix=a.fix,
        negative_control=a.negative_control,
    )


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
