#!/usr/bin/env python3
"""Loopback POP3 server + independent mailbox observer for the round-3 ladder.

Why not GreenMail: the precheck (NOT-STARTED.md) started
greenmail/standalone:2.1.14 and seeded a message with Python smtplib, but
async-pop 1.1.3 `login` hangs on GreenMail's chunked CAPA reply (the client's
decode_needs is not reset after a successful decode, so the next +OK-only
response waits for bytes that never come).  That is a client bug, not a
server-availability problem; the fallback allowed by the round-3 brief is this
real-protocol loopback POP3 server.  See RESULTS-FRAGMENT.md.

Protocol: real POP3 commands and state (USER/PASS/CAPA/STAT/RETR/RSET/NOOP/
QUIT), one message, dot-stuffed multiline RETR.  Every response is written in
one call so the parser sees a whole response.

Independent observer: a separate HTTP server (second loopback port) that reads
the server-owned mailbox state.  It does not go through the tested handle or
the proxy.

Usage: python3 pop_server.py --pop-port 0 --obs-port 0 --log FILE
Prints "POP_PORT=<n> OBS_PORT=<n>" then "READY".
"""
import argparse
import asyncio
import json
import time

SUBJECT = "dw-r3-pop-seed"
MESSAGE = (
    "From: seed@example.com\r\n"
    "To: popuser@example.com\r\n"
    f"Subject: {SUBJECT}\r\n"
    "Content-Type: text/plain; charset=utf-8\r\n"
    "\r\n"
    "dw-r3-pop-body for cancellation research\r\n"
).encode()
# POP3 octet count and dot-stuffed wire form.
SIZE = len(MESSAGE)
WIRE_LINES = []
for line in MESSAGE.split(b"\r\n"):
    if line.startswith(b"."):
        line = b"." + line
    WIRE_LINES.append(line)
RETR_BODY = b"+OK %d octets\r\n" % SIZE + b"\r\n".join(WIRE_LINES) + b".\r\n"


class State:
    def __init__(self, log_path):
        self.log_path = log_path
        self.messages = 1
        self.subject = SUBJECT
        self.sessions = 0
        self.commands = []
        self.retr_served = 0

    def record(self, session, line, result):
        if line.upper().startswith(b"RETR"):
            self.retr_served += 1
        self.commands.append({
            "t": round(time.monotonic(), 6),
            "session": session,
            "cmd": line.decode(errors="replace"),
            "result": result,
        })
        with open(self.log_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(self.commands[-1], sort_keys=True) + "\n")


async def pop_session(reader, writer, state):
    state.sessions += 1
    session = state.sessions
    authenticated = False
    try:
        writer.write(b"+OK dw-r3 loopback POP3 ready\r\n")
        await writer.drain()
        while True:
            raw = await reader.readline()
            if not raw:
                break
            line = raw.rstrip(b"\r\n")
            cmd = line.split(None, 1)[0].upper()
            if cmd == b"CAPA":
                writer.write(b"+OK\r\nUIDL\r\n.\r\n")
                result = "+OK capabilities"
            elif cmd == b"USER":
                writer.write(b"+OK\r\n")
                result = "+OK"
            elif cmd == b"PASS":
                authenticated = True
                writer.write(b"+OK\r\n")
                result = "+OK"
            elif cmd == b"STAT":
                if not authenticated:
                    writer.write(b"-ERR not authenticated\r\n")
                    result = "-ERR not authenticated"
                else:
                    writer.write(b"+OK %d %d\r\n" % (state.messages, SIZE))
                    result = "+OK"
            elif cmd == b"RETR":
                if not authenticated:
                    writer.write(b"-ERR not authenticated\r\n")
                    result = "-ERR not authenticated"
                else:
                    writer.write(RETR_BODY)
                    result = "+OK %d octets" % SIZE
            elif cmd == b"RSET":
                writer.write(b"+OK\r\n" if authenticated else b"-ERR not authenticated\r\n")
                result = "+OK" if authenticated else "-ERR not authenticated"
            elif cmd == b"NOOP":
                writer.write(b"+OK\r\n")
                result = "+OK"
            elif cmd == b"QUIT":
                writer.write(b"+OK bye\r\n")
                result = "+OK bye"
            else:
                writer.write(b"-ERR unknown command\r\n")
                result = "-ERR unknown"
            state.record(session, line, result)
            await writer.drain()
            if cmd == b"QUIT":
                break
    except (ConnectionError, asyncio.IncompleteReadError):
        pass
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass
        state.record(session, b"<closed>", "closed")


async def observer(reader, writer, state):
    try:
        head = await reader.read(4096)
        path = head.split(b" ")[1] if b" " in head else b"/"
        body = json.dumps({
            "messages": state.messages,
            "subject": state.subject,
            "sessions": state.sessions,
            "retr_served": state.retr_served,
            "commands": state.commands[-8:],
        }, sort_keys=True).encode()
        status = b"200 OK" if path in (b"/", b"/state") else b"404 Not Found"
        writer.write(b"HTTP/1.0 " + status + b"\r\nContent-Type: application/json\r\n"
                     b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body)
        await writer.drain()
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pop-port", type=int, default=0)
    ap.add_argument("--obs-port", type=int, default=0)
    ap.add_argument("--log", required=True)
    args = ap.parse_args()
    state = State(args.log)
    pop = await asyncio.start_server(
        lambda r, w: pop_session(r, w, state), "127.0.0.1", args.pop_port)
    obs = await asyncio.start_server(
        lambda r, w: observer(r, w, state), "127.0.0.1", args.obs_port)
    pop_port = pop.sockets[0].getsockname()[1]
    obs_port = obs.sockets[0].getsockname()[1]
    print(f"POP_PORT={pop_port} OBS_PORT={obs_port}", flush=True)
    print("READY", flush=True)
    async with pop, obs:
        await asyncio.gather(pop.serve_forever(), obs.serve_forever())


if __name__ == "__main__":
    asyncio.run(main())
