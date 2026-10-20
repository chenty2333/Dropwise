"""Shared helpers for the Python retro cases: argument parsing and delay proxy."""

from __future__ import annotations

import argparse
import asyncio
import contextlib


class DelayProxy:
    """A loopback TCP proxy that delays each forwarded chunk.

    It makes a server reply arrive after the client has already suspended, so
    cancelling at a suspension point leaves a reply genuinely unread (rather
    than one that was already buffered and would be caught by the pool's
    non-destructive peek).  Same idea as the proxy in redis-py#2665.
    """

    def __init__(self, host: str, port: int, delay: float = 0.05) -> None:
        self.host = host
        self.port = port
        self.delay = delay
        self._server: asyncio.AbstractServer | None = None
        self._tasks: set[asyncio.Task] = set()
        self._writers: set[asyncio.StreamWriter] = set()

    async def start(self) -> int:
        self._server = await asyncio.start_server(self._handle, "127.0.0.1", 0)
        assert self._server.sockets
        return self._server.sockets[0].getsockname()[1]

    async def _handle(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self._writers.add(writer)
        try:
            up_reader, up_writer = await asyncio.open_connection(self.host, self.port)
        except BaseException:
            writer.close()
            self._writers.discard(writer)
            return
        self._writers.add(up_writer)
        task = asyncio.current_task()
        assert task is not None
        self._tasks.add(task)
        try:
            await asyncio.gather(
                self._pipe(reader, up_writer),
                self._pipe(up_reader, writer),
            )
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            self._tasks.discard(task)
            for w in (writer, up_writer):
                with contextlib.suppress(BaseException):
                    w.close()
                self._writers.discard(w)

    async def _pipe(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                if self.delay:
                    await asyncio.sleep(self.delay)
                writer.write(data)
                await writer.drain()
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            with contextlib.suppress(BaseException):
                writer.close()

    async def close(self) -> None:
        if self._server is not None:
            self._server.close()
            with contextlib.suppress(BaseException):
                await self._server.wait_closed()
            self._server = None
        for task in list(self._tasks):
            task.cancel()
        for task in list(self._tasks):
            with contextlib.suppress(BaseException):
                await task
        for writer in list(self._writers):
            with contextlib.suppress(BaseException):
                writer.close()
        self._tasks.clear()
        self._writers.clear()


def base_parser(description: str) -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=description)
    p.add_argument("--url", required=True, help="direct server URL (observer path)")
    p.add_argument(
        "--levels",
        default="B1,B2,B3,M1",
        help="comma-separated subset of B1,B2,B3,M1",
    )
    p.add_argument("--reps", type=int, default=3, help="repetitions per plan")
    p.add_argument("--timeout", type=float, default=15.0, help="per-operation timeout")
    p.add_argument("--round", type=int, default=0, help="round label for logs")
    p.add_argument("--target", default=None, help="case-specific target name")
    return p


def compact(report) -> str:
    return (
        f"SUMMARY level={report.level} yields={report.target_yields} "
        f"trials={len(report.trials)} violations={len(report.violations())} "
        f"unknown={len(report.unknown())} unrealized={len(report.unrealized())} "
        f"seconds={report.seconds:.2f}"
    )
