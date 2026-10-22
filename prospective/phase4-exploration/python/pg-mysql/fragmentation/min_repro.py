"""Minimal reproduction & verification: psycopg cancel during a blocked send.

Verifies:
1. Cancellation during a blocked partial send fails closed.
2. Independent observer records:
   - Client-side close timing;
   - Proxy upstream socket close timing (proxy remains alive to distinguish driver
     vs proxy delays);
   - Server-side backend process termination in pg_stat_activity (bounded wait);
   - Server-side lock clearance in pg_locks;
   - Independent table lock acquisition and uncorrupted row state.

Run (from prospective/phase4-exploration/python/pg-mysql):
  PYTHONPATH=".:$REPO/research/python-retro" \
    ~/dropwise-py-work-r3/venv313/bin/python -m fragmentation.min_repro --port 32919
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import sys
import time

import psycopg

from fragmentation.fragment_proxy import FragmentingProxy, send_q

PAYLOAD = "x" * 8_000_000
TABLE_NAME = "test_min_repro_cleanup"


async def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=32919)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--fix", action="store_true")
    a = p.parse_args()
    if a.fix:
        from fragmentation.partial_send_fix import install

        install()

    observer = None
    proxy = None
    conn = None
    try:
        # 1. Independent observer (direct connection, NOT via proxy)
        observer = await psycopg.AsyncConnection.connect(
            f"host={a.host} port={a.port} user={a.user} password={a.password} "
            f"dbname={a.dbname} sslmode=disable",
            autocommit=True,
        )
        await observer.execute(f"DROP TABLE IF EXISTS {TABLE_NAME}")
        await observer.execute(
            f"CREATE TABLE {TABLE_NAME} (id int primary key, payload text)"
        )

        # 2. Start proxy
        proxy = FragmentingProxy(a.host, a.port, delay=0.0, mode="off")
        proxy_port = await proxy.start()

        # 3. Connect tested client via proxy
        conn = await psycopg.AsyncConnection.connect(
            f"host={a.host} port={proxy_port} user={a.user} password={a.password} "
            f"dbname={a.dbname} sslmode=disable",
            autocommit=True,
        )
        tested_pid = conn.pgconn.backend_pid
        fd = conn.pgconn.socket

        # Verify tested PID is visible server-side
        cur = await observer.execute(
            "SELECT count(*) FROM pg_stat_activity WHERE pid = %s", (tested_pid,)
        )
        assert (await cur.fetchone())[0] == 1, (
            "Tested PID must be active in pg_stat_activity"
        )
        print(f"tested connection backend PID: {tested_pid}")

        # 4. Partial send with proxy backpressure hold
        proxy.arm_hold(65536)
        task = asyncio.create_task(
            conn.execute(f"INSERT INTO {TABLE_NAME} VALUES (1, %s)", (PAYLOAD,))
        )
        await asyncio.wait_for(proxy.hold_reached.wait(), 5)
        await asyncio.sleep(0.3)
        print(
            f"before cancel: done={task.done()} send_q={send_q(fd)} "
            f"proxy_bytes={proxy.snapshot()['bytes_c2s']} "
            f"flush={conn.pgconn.flush()} tx={conn.info.transaction_status}",
            flush=True,
        )

        # 5. Cancel task
        t_cancel_start = time.monotonic()
        task.cancel()
        proxy.release_hold()

        try:
            await asyncio.wait_for(task, 10)
            print("UNEXPECTED: task completed", task.result(), flush=True)
        except BaseException as exc:
            print(
                f"cancelled at {time.monotonic() - t_cancel_start:.3f}s -> "
                f"{type(exc).__name__}: {exc}",
                flush=True,
            )

        # 6. Record client-side close
        t_client_close_start = time.monotonic()
        if not conn.closed:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(conn.close(), 3)
        t_client_closed = time.monotonic()
        client_close_duration = t_client_closed - t_client_close_start
        print(
            f"client closed: {conn.closed} (took {client_close_duration:.4f}s)",
            flush=True,
        )

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
            print("WARNING: proxy upstream connection did not close within 3s", flush=True)

        # 8. Bounded wait for server-side backend termination & locks release via observer
        backend_gone = False
        t_backend_gone = None
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            cur = await observer.execute(
                "SELECT count(*) FROM pg_stat_activity WHERE pid = %s", (tested_pid,)
            )
            act_cnt = (await cur.fetchone())[0]
            cur = await observer.execute(
                "SELECT count(*) FROM pg_locks WHERE pid = %s", (tested_pid,)
            )
            lock_cnt = (await cur.fetchone())[0]
            if act_cnt == 0 and lock_cnt == 0:
                backend_gone = True
                t_backend_gone = time.monotonic()
                break
            await asyncio.sleep(0.05)

        if not backend_gone:
            print(
                f"ERROR: backend cleanup UNCONFIRMED for PID {tested_pid} after 5.0s!",
                flush=True,
            )
            return 1

        backend_cleanup_delay = t_backend_gone - (t_proxy_upstream or t_client_closed)
        print(
            f"server backend gone: True (+{backend_cleanup_delay:.4f}s after upstream close; "
            f"observer confirmed 0 in pg_stat_activity, 0 in pg_locks)",
            flush=True,
        )

        # 9. Independent lock and table integrity check
        async with observer.transaction():
            await asyncio.wait_for(
                observer.execute(
                    f"LOCK TABLE {TABLE_NAME} IN ACCESS EXCLUSIVE MODE NOWAIT"
                ),
                timeout=2.0,
            )
        cur = await observer.execute(f"SELECT count(*) FROM {TABLE_NAME}")
        cnt = (await cur.fetchone())[0]
        print(
            f"independent lock check: ACCESS EXCLUSIVE acquired; table row count: {cnt} (expected 0)",
            flush=True,
        )
        if cnt != 0:
            print(f"ERROR: cancelled insert committed {cnt} rows!", flush=True)
            return 1

        return 0

    finally:
        if observer is not None and not observer.closed:
            with contextlib.suppress(BaseException):
                await observer.execute(f"DROP TABLE IF EXISTS {TABLE_NAME}")
                await observer.close()
        if proxy is not None:
            await proxy.close()


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
