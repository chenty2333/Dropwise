"""A DelayProxy variant with finite, counted PostgreSQL server->client splits.

Modes (only the server->client direction is split):
  off       exactly DelayProxy's behaviour (one delayed write per read chunk)
  boundary  write one complete protocol message, pause, then the next message
  header    write 3 bytes of the 5-byte message header (inside the 4-byte
            length field), pause, then the rest of that message
  payload   write the 5-byte header plus 2 payload bytes, pause, then the rest
            (messages without payload fall back to the boundary behaviour)

Every write is counted.  The currently delivered message and split offset are
exposed in ``snapshot()`` so the cut injection can record where it landed; the
same object counts client->server bytes and can hold (stop reading) after a
byte threshold for the send-direction backpressure experiment.

This is deliberately not a general protocol parser: it only reads the v3
type/length header to find message boundaries.  Anything that does not look
like a v3 message (e.g. the 1-byte SSL refusal during connect, which is not
fragmented here) is forwarded without splitting and counted as a fallback.
"""

from __future__ import annotations

import asyncio
import contextlib
import fcntl
import socket
import struct
import termios
import time

from cases.common import DelayProxy

# Server v3 message type bytes that actually occur in the flows used here.
_TYPES = b"R S K Z T D C E N A 1 2 3 n s I G H d c V v".replace(b" ", b"")
_SIOCOUTQ = 0x5411


def fd_bytes(fd: int, request: int) -> int | None:
    """ioctl on a duplicated fd; never closes the driver's descriptor."""
    try:
        dup = socket.fromfd(fd, socket.AF_INET, socket.SOCK_STREAM)
    except OSError:
        return None
    try:
        raw = fcntl.ioctl(dup.fileno(), request, b"\0\0\0\0")
        return struct.unpack("i", raw)[0]
    except OSError:
        return None
    finally:
        dup.close()


def recv_q(fd: int) -> int | None:
    """Bytes in the kernel receive queue, not yet read by the client."""
    return fd_bytes(fd, termios.FIONREAD)


def send_q(fd: int) -> int | None:
    """Bytes still queued on the client's send side (SIOCOUTQ)."""
    return fd_bytes(fd, _SIOCOUTQ)


class FragmentingProxy(DelayProxy):
    def __init__(
        self,
        host: str,
        port: int,
        delay: float = 0.0,
        mode: str = "off",
        pause: float = 0.15,
        header_split: int = 3,
        payload_split: int = 7,
    ) -> None:
        super().__init__(host, port, delay)
        self.mode = mode
        self.pause = pause
        self.header_split = header_split
        self.payload_split = payload_split

        self.bytes_s2c = 0
        self.bytes_c2s = 0
        self.writes_s2c = 0
        self.writes_c2s = 0
        self.frames = 0
        self.splits: list[dict] = []
        self.current: dict | None = None
        self.pause_active = False
        self.waiting = asyncio.Event()  # set while a prefix is written and paused
        self.pause_count = 0
        self.parse_fallback = 0
        self.hold_after: int | None = None
        self.hold_reached = asyncio.Event()
        self.hold_gate = asyncio.Event()
        self.hold_gate.set()

    # ---- accountancy -------------------------------------------------
    def reset_stats(self) -> None:
        self.bytes_s2c = self.bytes_c2s = 0
        self.writes_s2c = self.writes_c2s = 0
        self.frames = self.pause_count = self.parse_fallback = 0
        self.splits.clear()
        self.current = None
        self.pause_active = False
        self.waiting.clear()
        self.hold_after = None
        self.hold_reached.clear()
        self.hold_gate.set()

    def arm_hold(self, after_bytes: int) -> None:
        """Stop reading the client direction after this many forwarded bytes."""
        self.hold_after = after_bytes
        self.hold_reached.clear()
        self.hold_gate.clear()

    def release_hold(self) -> None:
        self.hold_gate.set()

    def snapshot(self) -> dict:
        return {
            "mode": self.mode,
            "bytes_s2c": self.bytes_s2c,
            "writes_s2c": self.writes_s2c,
            "bytes_c2s": self.bytes_c2s,
            "writes_c2s": self.writes_c2s,
            "frames": self.frames,
            "pause_count": self.pause_count,
            "pause_active": self.pause_active,
            "current": None if self.current is None else dict(self.current),
            "last_splits": [dict(x) for x in self.splits[-4:]],
            "parse_fallback": self.parse_fallback,
            "hold_active": not self.hold_gate.is_set(),
            "hold_reached": self.hold_reached.is_set(),
        }

    # ---- connection wiring ------------------------------------------
    async def _handle(self, reader, writer) -> None:
        # Same structure as DelayProxy._handle, but direction-specific pipes.
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
                self._pipe_c2s(reader, up_writer),
                self._pipe_s2c(up_reader, writer),
            )
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            self._tasks.discard(task)
            for w in (writer, up_writer):
                with contextlib.suppress(BaseException):
                    w.close()
                self._writers.discard(w)

    async def _write(self, writer, data: bytes, direction: str) -> None:
        writer.write(data)
        await writer.drain()
        if direction == "s2c":
            self.bytes_s2c += len(data)
            self.writes_s2c += 1
        else:
            self.bytes_c2s += len(data)
            self.writes_c2s += 1

    async def _pipe_c2s(self, reader, writer) -> None:
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                if self.delay:
                    await asyncio.sleep(self.delay)
                await self._write(writer, data, "c2s")
                if (
                    self.hold_after is not None
                    and not self.hold_reached.is_set()
                    and self.bytes_c2s >= self.hold_after
                ):
                    # Stop reading the client: its send queue and this socket's
                    # receive queue fill, so the client's flush cannot finish.
                    self.hold_reached.set()
                    await self.hold_gate.wait()
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            with contextlib.suppress(BaseException):
                writer.close()

    @staticmethod
    def _frame(buf: bytearray):
        if len(buf) < 5:
            return None
        typ = buf[0]
        if typ not in _TYPES:
            return "bad"
        length = int.from_bytes(buf[1:5], "big")
        if length < 4 or length > (1 << 24):
            return "bad"
        total = 1 + length
        if len(buf) < total:
            return None
        return bytes(buf[:total])

    async def _pipe_s2c(self, reader, writer) -> None:
        buf = bytearray()
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                if self.mode == "off":
                    # exactly the parent's chunk-delay behaviour
                    if self.delay:
                        await asyncio.sleep(self.delay)
                    await self._write(writer, data, "s2c")
                    continue
                buf += data
                while True:
                    frame = self._frame(buf)
                    if frame is None:
                        break
                    if frame == "bad":
                        self.parse_fallback += 1
                        await self._write(writer, bytes(buf), "s2c")
                        buf.clear()
                        break
                    del buf[: len(frame)]
                    await self._emit(writer, frame)
        except (asyncio.CancelledError, ConnectionError, OSError):
            pass
        finally:
            with contextlib.suppress(BaseException):
                writer.close()

    async def _emit(self, writer, frame: bytes) -> None:
        self.frames += 1
        typ = chr(frame[0])
        ln = len(frame)
        if self.mode == "boundary" or (
            self.mode == "payload" and ln <= self.payload_split
        ) or (self.mode == "header" and ln <= self.header_split):
            cut, kind = ln, "boundary"
        elif self.mode == "header":
            cut, kind = self.header_split, "header"
        else:
            cut, kind = self.payload_split, "payload"
        prefix, rest = frame[:cut], frame[cut:]
        self.current = {
            "type": typ,
            "len": ln,
            "cut": cut,
            "sent": 0,
            "kind": kind,
            "t": round(time.monotonic(), 6),
        }
        await self._write(writer, prefix, "s2c")
        self.current["sent"] = cut
        self.splits.append(dict(self.current))
        self.pause_count += 1
        self.pause_active = True
        self.waiting.set()
        await asyncio.sleep(self.pause)
        self.pause_active = False
        self.waiting.clear()
        if rest:
            await self._write(writer, rest, "s2c")
            self.current["sent"] = ln
        # Keep the completed frame visible until the next one starts.
        if len(self.splits) > 200:
            del self.splits[:100]
