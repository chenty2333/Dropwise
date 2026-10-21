"""Standalone loopback TCP proxy with per-direction delay and PG frame log.

Usage:
    python proxy.py --target-port 5432 [--c2s-delay 0] [--s2c-delay 0.05] [--log file]

Prints "PORT <listen-port>" on stdout once listening, then serves until killed.
Frame parsing is best-effort: startup/SSLRequest, then regular PG messages.
Log lines are JSON: {"t":..., "dir":"c2s"|"s2c", "type":"Q", "text":...}
"""
import argparse
import json
import selectors
import socket
import sys
import time
import os

SSL_REQUEST = 80877103


class FrameLog:
    def __init__(self, path):
        self.f = open(path, "a", buffering=1) if path else None

    def write(self, direction, kind, **fields):
        if not self.f:
            return
        rec = {"t": round(time.monotonic(), 6), "dir": direction, "type": kind}
        rec.update(fields)
        self.f.write(json.dumps(rec, default=str) + "\n")


class StreamParser:
    """Incremental frontend/backend PostgreSQL message parser (best effort)."""

    def __init__(self, direction, log):
        self.direction = direction
        self.log = log
        self.buf = b""
        self.startup_done = False
        self.ssl_done = False

    def feed(self, data, offset):
        self.buf += data
        while True:
            if not self.startup_done and self.direction == "c2s":
                if len(self.buf) < 4:
                    return
                n = int.from_bytes(self.buf[:4], "big")
                if n < 4 or n > 1 << 20:
                    self.startup_done = True
                    continue
                if len(self.buf) < n:
                    return
                body = self.buf[4:n]
                self.buf = self.buf[n:]
                code = int.from_bytes(body[:4], "big") if len(body) >= 4 else 0
                if code == SSL_REQUEST:
                    self.log.write(self.direction, "SSLRequest", t_off=offset)
                    self.ssl_done = True
                elif code == 80877104:
                    self.log.write(self.direction, "GSSENCRequest", t_off=offset)
                    self.ssl_done = True
                elif code == 80877102:
                    self.log.write(self.direction, "CancelRequest", t_off=offset)
                    self.startup_done = True
                else:
                    self.log.write(self.direction, "Startup", protocol=code, t_off=offset)
                    self.startup_done = True
                continue
            if not self.startup_done and self.direction == "s2c":
                # first byte may be 'N'/'S' for SSL/GSS, or a normal message type
                if not self.buf:
                    return
                if self.buf[:1] in (b"N", b"S") and len(self.buf) == 1:
                    self.log.write(self.direction, self.buf[:1].decode(), t_off=offset)
                    self.buf = b""
                    continue
                if self.buf[:1] == b"E" and len(self.buf) >= 5:
                    # could be an error response after SSLRequest
                    pass
                self.startup_done = True
                continue
            if len(self.buf) < 5:
                return
            mtype = self.buf[:1].decode("latin1")
            n = int.from_bytes(self.buf[1:5], "big")
            if n < 4 or n > 1 << 24:
                self.log.write(self.direction, "corrupt", head=self.buf[:16].hex())
                self.buf = b""
                return
            if len(self.buf) < n + 1:
                return
            payload = self.buf[5 : n + 1]
            self.buf = self.buf[n + 1 :]
            self.log.write(self.direction, mtype, t_off=offset,
                           **self._describe(mtype, payload))

    def _describe(self, mtype, payload):
        out = {"len": len(payload)}
        if mtype == "Q":
            out["text"] = payload.rstrip(b"\x00").decode("utf-8", "replace")[:200]
        elif mtype == "P":
            # Parse: statement name\0 query\0
            parts = payload.split(b"\x00")
            if len(parts) >= 2:
                out["text"] = parts[1].decode("utf-8", "replace")[:200]
        elif mtype in ("C", "Z", "E", "R", "S", "K", "N"):
            if mtype == "C":
                out["tag"] = payload.rstrip(b"\x00").decode("utf-8", "replace")[:80]
            elif mtype == "Z":
                out["status"] = chr(payload[0]) if payload else "?"
            elif mtype == "E":
                out["error"] = payload[:160].decode("utf-8", "replace")
            elif mtype == "S":
                parts = payload.split(b"\x00")
                out["name"] = parts[0].decode("utf-8", "replace")
            elif mtype == "R":
                out["auth"] = int.from_bytes(payload[:4], "big") if len(payload) >= 4 else None
        return out


class Conn:
    def __init__(self, client, server, log, c2s_delay, s2c_delay):
        self.client, self.server = client, server
        self.c2s_delay, self.s2c_delay = c2s_delay, s2c_delay
        self.log = log
        self.parsers = {"c2s": StreamParser("c2s", log), "s2c": StreamParser("s2c", log)}
        self.pending = []  # (due, target_sock, data, direction)

    def handle(self, sock):
        try:
            data = sock.recv(1 << 16)
        except OSError:
            data = b""
        direction = "c2s" if sock is self.client else "s2c"
        if not data:
            return False
        target = self.server if direction == "c2s" else self.client
        delay = self.c2s_delay if direction == "c2s" else self.s2c_delay
        self.parsers[direction].feed(data, round(time.monotonic(), 6))
        self.pending.append((time.monotonic() + delay, target, data, direction))
        return True

    def flush(self):
        now = time.monotonic()
        rest = []
        for due, target, data, direction in self.pending:
            if due <= now:
                try:
                    target.sendall(data)
                except OSError:
                    return False
            else:
                rest.append((due, target, data, direction))
        self.pending = rest
        return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--target-host", default="127.0.0.1")
    ap.add_argument("--target-port", type=int, required=True)
    ap.add_argument("--c2s-delay", type=float, default=0.0)
    ap.add_argument("--s2c-delay", type=float, default=0.0)
    ap.add_argument("--log", default=None)
    args = ap.parse_args()

    log = FrameLog(args.log)
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 0))
    srv.listen(16)
    print(f"PORT {srv.getsockname()[1]}", flush=True)

    sel = selectors.DefaultSelector()
    sel.register(srv, selectors.EVENT_READ, ("accept", None))
    conns = {}
    while True:
        timeout = 0.005
        for key, _ in sel.select(timeout):
            tag, conn = key.data
            if tag == "accept":
                client, _ = srv.accept()
                client.setblocking(False)
                try:
                    server = socket.create_connection((args.target_host, args.target_port))
                except OSError:
                    client.close()
                    continue
                server.setblocking(False)
                c = Conn(client, server, log, args.c2s_delay, args.s2c_delay)
                conns[client.fileno()] = c
                sel.register(client, selectors.EVENT_READ, ("data", c))
                sel.register(server, selectors.EVENT_READ, ("data", c))
            else:
                sock_obj = key.fileobj
                alive = conn.handle(sock_obj)
                if not alive:
                    try:
                        sel.unregister(conn.client); sel.unregister(conn.server)
                    except (KeyError, ValueError):
                        pass
                    conns.pop(conn.client.fileno(), None)
                    conn.client.close(); conn.server.close()
        for conn in list(conns.values()):
            if not conn.flush():
                try:
                    sel.unregister(conn.client); sel.unregister(conn.server)
                except (KeyError, ValueError):
                    pass
                conns.pop(conn.client.fileno(), None)
                conn.client.close(); conn.server.close()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
