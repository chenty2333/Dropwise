"""Instrumented loopback delay proxy for the mysql-connector recovery-delay case.

Subclasses the read-only `DelayProxy` from `cases/common.py` (which itself
loads `research/python-retro/cases/common.py`) and adds:

* a mutable **server->client** delay (`set_delay`), so a scenario can run its
  setup quickly and then switch to 50 ms / 5 s for the target reply;
* an ordered local event log with monotonic timestamps for every forwarded
  byte chunk (`c2s` = client->server, `s2c_read` = read from server, before the
  delay; `s2c_deliver` = written to the client, after the delay);
* a `wait_s2c_read(marker, timeout)` helper that lets the caller cancel at the
  instant a server reply has been read by the proxy and is being held.

The proxy only forwards bytes; it does not parse the MySQL protocol.  Byte
counts and timestamps are evidence of direction/blocking, not of statement
identity.  `docs` in the case module derive statement identity from the
connector's own call site plus the independent observer.
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
import time

from cases.common import DelayProxy as RetroDelayProxy


class Chunk:
    __slots__ = ("seq", "conn", "direction", "bytes", "read_t", "delay",
                 "deliver_t", "delivered", "error")

    def __init__(self, seq, conn, direction, nbytes, read_t, delay):
        self.seq = seq
        self.conn = conn
        self.direction = direction
        self.bytes = nbytes
        self.read_t = read_t
        self.delay = delay
        self.deliver_t = None
        self.delivered = False
        self.error = None

    def as_dict(self) -> dict:
        return {
            "seq": self.seq,
            "conn": self.conn,
            "dir": self.direction,
            "bytes": self.bytes,
            "read_t": round(self.read_t, 6),
            "delay": self.delay,
            "deliver_t": None if self.deliver_t is None else round(self.deliver_t, 6),
            "delivered": self.delivered,
            "error": self.error,
        }


class DelayProxy(RetroDelayProxy):
    """Loopback proxy with a controllable s2c delay and local event log."""

    def __init__(self, host: str, port: int, delay: float = 0.05) -> None:
        super().__init__(host, port, delay=delay)
        self.delay = float(delay)          # initial s2c delay
        self.delay_c2s = 0.0               # upstream delay is always 0 here
        self.events: list[dict] = []
        self.chunks: list[Chunk] = []
        self._seq = itertools.count(1)
        self._conn_seq = itertools.count(1)
        self._s2c_read_seq = 0
        self._s2c_event = asyncio.Event()
        self.open_conns = 0
        self.closed_conns = 0
        self.conn_open_t: dict[int, float] = {}
        self.conn_close_t: dict[int, float] = {}
        # Direction-level close evidence; `_handle` cancels the sibling pipe
        # as soon as one side ends, so a held 5 s reply does not delay the
        # upstream FIN that makes the server roll back.
        self.upstream_close_t: dict[int, float] = {}
        self.client_close_t: dict[int, float] = {}

    # ---- control -----------------------------------------------------
    def set_delay(self, delay: float) -> None:
        """Change the server->client delay (seconds) for subsequent chunks."""
        self.delay = float(delay)

    def mark(self) -> dict:
        """A phase marker: chunk/event/s2c sequence counters at this instant."""
        return {
            "t": time.monotonic(),
            "chunks": len(self.chunks),
            "events": len(self.events),
            "s2c_read_seq": self._s2c_read_seq,
        }

    def s2c_marker(self) -> int:
        return self._s2c_read_seq

    async def wait_s2c_read(self, marker: int, timeout: float) -> int:
        """Wait until a server->client chunk has been read after `marker`."""
        async def _wait() -> int:
            while True:
                if self._s2c_read_seq > marker:
                    return self._s2c_read_seq
                self._s2c_event.clear()
                if self._s2c_read_seq > marker:
                    return self._s2c_read_seq
                await self._s2c_event.wait()

        return await asyncio.wait_for(_wait(), timeout)

    # ---- instrumentation --------------------------------------------
    def _event(self, name: str, **fields) -> None:
        rec = {"t": round(time.monotonic(), 6), "event": name}
        rec.update(fields)
        self.events.append(rec)

    def _record(self, conn: int, direction: str, nbytes: int, read_t: float,
                delay: float) -> Chunk:
        chunk = Chunk(next(self._seq), conn, direction, nbytes, read_t, delay)
        self.chunks.append(chunk)
        if direction == "s2c":
            self._s2c_read_seq += 1
            self._event("s2c_read", conn=conn, bytes=nbytes, delay=delay,
                        chunk=chunk.seq)
            self._s2c_event.set()
        return chunk

    # ---- server ------------------------------------------------------
    async def _handle(self, reader, writer) -> None:
        conn = next(self._conn_seq)
        self.open_conns += 1
        self.conn_open_t[conn] = time.monotonic()
        self._event("conn_open", conn=conn, open_conns=self.open_conns)
        self._writers.add(writer)
        try:
            up_reader, up_writer = await asyncio.open_connection(self.host, self.port)
        except BaseException as exc:
            writer.close()
            self._writers.discard(writer)
            self.open_conns -= 1
            self._event("conn_failed", conn=conn, error=type(exc).__name__)
            return
        self._writers.add(up_writer)
        task = asyncio.current_task()
        assert task is not None
        self._tasks.add(task)
        pipes = [
            asyncio.create_task(self._pipe_c2s(conn, reader, up_writer)),
            asyncio.create_task(self._pipe_s2c(conn, up_reader, writer)),
        ]
        try:
            await asyncio.wait(pipes, return_when=asyncio.FIRST_COMPLETED)
        except asyncio.CancelledError:
            pass
        finally:
            for pipe in pipes:
                if not pipe.done():
                    pipe.cancel()
            for pipe in pipes:
                with contextlib.suppress(BaseException):
                    await pipe
            self._tasks.discard(task)
            for w in (writer, up_writer):
                with contextlib.suppress(BaseException):
                    w.close()
                self._writers.discard(w)
            self.open_conns -= 1
            self.closed_conns += 1
            self.conn_close_t[conn] = time.monotonic()
            self._event("conn_close", conn=conn, open_conns=self.open_conns)

    async def _pipe_c2s(self, conn, reader, writer) -> None:
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                if self.delay_c2s:
                    await asyncio.sleep(self.delay_c2s)
                chunk = self._record(conn, "c2s", len(data), time.monotonic(), 0.0)
                chunk.delivered = True
                chunk.deliver_t = time.monotonic()
                writer.write(data)
                await writer.drain()
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            self.upstream_close_t[conn] = time.monotonic()
            self._event("upstream_close", conn=conn)
            with contextlib.suppress(BaseException):
                writer.close()

    async def _pipe_s2c(self, conn, reader, writer) -> None:
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                delay = self.delay
                chunk = self._record(conn, "s2c", len(data), time.monotonic(), delay)
                if delay:
                    await asyncio.sleep(delay)
                writer.write(data)
                await writer.drain()
                chunk.delivered = True
                chunk.deliver_t = time.monotonic()
                self._event("s2c_deliver", conn=conn, bytes=len(data),
                            delay=delay, chunk=chunk.seq)
        except asyncio.CancelledError:
            if "chunk" in locals():
                chunk.error = "proxy_cancelled"
            raise
        except (ConnectionError, OSError) as exc:
            if "chunk" in locals():
                chunk.error = f"{type(exc).__name__}: {exc}"
        finally:
            self.client_close_t[conn] = time.monotonic()
            self._event("client_close", conn=conn)
            with contextlib.suppress(BaseException):
                writer.close()

    async def close(self) -> None:
        self._event("proxy_close", open_conns=self.open_conns)
        await super().close()

    # ---- reporting ---------------------------------------------------
    def snapshot(self, include_chunks: bool = True) -> dict:
        held = [c for c in self.chunks if c.direction == "s2c"
                and not c.delivered and c.error is None]
        out = {
            "open_conns": self.open_conns,
            "closed_conns": self.closed_conns,
            "s2c_read_seq": self._s2c_read_seq,
            "chunks_total": len(self.chunks),
            "held_s2c": [
                {"seq": c.seq, "conn": c.conn, "bytes": c.bytes,
                 "read_t": round(c.read_t, 6), "delay": c.delay}
                for c in held
            ],
            "conn_open_t": {k: round(v, 6) for k, v in self.conn_open_t.items()},
            "conn_close_t": {k: round(v, 6) for k, v in self.conn_close_t.items()},
            "upstream_close_t": {k: round(v, 6)
                                 for k, v in self.upstream_close_t.items()},
            "client_close_t": {k: round(v, 6)
                               for k, v in self.client_close_t.items()},
            "events": list(self.events),
        }
        if include_chunks:
            out["chunks"] = [c.as_dict() for c in self.chunks]
        return out

    def chunk_window(self, start: int, end: int | None = None) -> list[dict]:
        end = len(self.chunks) if end is None else end
        return [c.as_dict() for c in self.chunks[start:end]]
