"""Minimal reproduction: psycopg cancel during a blocked (partial) send.

Proxy: forwards the PostgreSQL handshake, then forwards at most 64 KiB of the
client->server direction and stops reading.  The client's socket send queue
fills, so `conn.execute()` is suspended with the request only partly written.
Cancelling there is expected to raise CancelledError; on psycopg 3.3.6 it raises
TypeError and leaves the connection ACTIVE/busy.

Run (from prospective/phase4-exploration/python/pg-mysql):
  PYTHONPATH=".:$REPO/research/python-retro" \
    ~/dropwise-py-work-r3/venv313/bin/python -m fragmentation.min_repro --port 32890
"""

from __future__ import annotations

import argparse
import asyncio
import sys
import time
import traceback

import psycopg

from fragmentation.fragment_proxy import FragmentingProxy, send_q

PAYLOAD = "x" * 8_000_000


async def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--user", default="postgres")
    p.add_argument("--password", default="dw")
    p.add_argument("--dbname", default="dw")
    p.add_argument("--fix", action="store_true")
    a = p.parse_args()
    if a.fix:
        from fragmentation.partial_send_fix import install
        install()

    proxy = FragmentingProxy(a.host, a.port, delay=0.0, mode="off")
    port = await proxy.start()
    conn = await psycopg.AsyncConnection.connect(
        f"host={a.host} port={port} user={a.user} password={a.password} "
        f"dbname={a.dbname} sslmode=disable", autocommit=True)
    fd = conn.pgconn.socket
    try:
        proxy.arm_hold(65536)
        task = asyncio.create_task(conn.execute("SELECT length(%s)", (PAYLOAD,)))
        await asyncio.wait_for(proxy.hold_reached.wait(), 5)
        await asyncio.sleep(0.3)
        print(f"before cancel: done={task.done()} send_q={send_q(fd)} "
              f"proxy_bytes={proxy.snapshot()['bytes_c2s']} "
              f"flush={conn.pgconn.flush()} tx={conn.info.transaction_status}",
              flush=True)
        started = time.monotonic()
        task.cancel()
        proxy.release_hold()
        try:
            await asyncio.wait_for(task, 10)
            print("UNEXPECTED: task completed", task.result(), flush=True)
        except BaseException as exc:
            print(f"cancelled at {time.monotonic() - started:.3f}s -> "
                  f"{type(exc).__name__}: {exc}", flush=True)
            traceback.print_exception(type(exc), exc, exc.__traceback__)
        await asyncio.sleep(0.1)
        print(f"after: tx={conn.info.transaction_status} "
              f"busy={conn.pgconn.is_busy()} closed={conn.closed}", flush=True)
        try:
            cur = await asyncio.wait_for(conn.execute("SELECT 1"), 3)
            print("successor:", await cur.fetchone(), flush=True)
        except BaseException as exc:
            print(f"successor failed: {type(exc).__name__}: {exc}", flush=True)
        await asyncio.wait_for(conn.close(), 3)
        print(f"close ok; backend gone: {conn.closed}", flush=True)
        return 0
    finally:
        await proxy.close()


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
