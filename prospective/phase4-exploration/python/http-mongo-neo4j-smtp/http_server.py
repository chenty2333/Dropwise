#!/usr/bin/env python3
"""A loopback HTTP/1.1 keep-alive server used by the httpx and curl-cffi cases.

It is a separate process: the client under test talks to it through the
read-only ``DelayProxy`` from research/python-retro, while the independent
observer talks to it directly (no proxy).  The server owns the per-connection
request log and the document state, so the observation does not depend on the
tested client being able to read its own replies.

Protocol: one request at a time per connection, HTTP/1.1 keep-alive,
``Content-Length`` bodies only (no chunked, no pipelining).  Credentials: none.
Bind: 127.0.0.1 only.

Endpoints
---------
GET  /warm                         -> {"ok": true}
POST /doc/<id>  {"value": v, "req": r}
                                   -> append log, doc[id] = v,
                                      reply {"req": r, "stored": v, "conn": c}
GET  /doc/<id>?req=r               -> reply {"req": r, "value": <doc or null>,
                                             "conn": c}
GET  /__observe__?doc=<id>         -> {"doc": ..., "requests": [...],
                                       "connections": n, "conn_count": {...}}
GET  /__health__                   -> {"ok": true, "requests": n}

``X-Reply-Delay: <seconds>`` delays the response headers (environment probe;
the level ladder uses the proxy, not this header).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import signal
import sys
import time
from pathlib import Path


class State:
    def __init__(self) -> None:
        self.docs: dict[str, object] = {}
        self.log: list[dict] = []
        self.connections = 0
        self.requests = 0
        self.conn_requests: dict[int, int] = {}
        self.started = time.time()
        self.log_path: Path | None = None
        self._log_lock = asyncio.Lock()

    def append(self, entry: dict) -> None:
        self.log.append(entry)
        if self.log_path is not None:
            with self.log_path.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(entry, sort_keys=True) + "\n")


async def read_headers(reader: asyncio.StreamReader) -> tuple[str, dict[str, str]] | None:
    line = await reader.readline()
    if not line:
        return None
    try:
        request_line = line.decode("latin-1").strip()
    except UnicodeDecodeError:
        return None
    parts = request_line.split(" ")
    if len(parts) != 3:
        return None
    method, target, _version = parts
    headers: dict[str, str] = {}
    while True:
        raw = await reader.readline()
        if not raw or raw in (b"\r\n", b"\n"):
            break
        name, _, value = raw.decode("latin-1").partition(":")
        headers[name.strip().lower()] = value.strip()
    return f"{method} {target}", headers


async def reply(
    writer: asyncio.StreamWriter,
    status: int,
    payload: dict,
    *,
    delay: float = 0.0,
    split_body: float = 0.0,
    conn: int,
) -> None:
    if delay > 0:
        await asyncio.sleep(delay)
    body = json.dumps(payload, sort_keys=True).encode()
    reason = {200: "OK", 400: "Bad Request", 404: "Not Found", 405: "Method Not Allowed"}.get(
        status, "OK"
    )
    head = (
        f"HTTP/1.1 {status} {reason}\r\n"
        "Content-Type: application/json\r\n"
        f"Content-Length: {len(body)}\r\n"
        f"X-Conn-Id: {conn}\r\n"
        "Connection: keep-alive\r\n"
        "\r\n"
    ).encode()
    if split_body > 0:
        # Diagnostic only: headers first, body later, so a cancellation can be
        # aimed at the response-body await of a plain task.
        writer.write(head)
        await writer.drain()
        await asyncio.sleep(split_body)
        writer.write(body)
    else:
        writer.write(head + body)
    await writer.drain()


async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter, state: State) -> None:
    state.connections += 1
    conn = state.connections
    state.conn_requests[conn] = 0
    try:
        while True:
            parsed = await read_headers(reader)
            if parsed is None:
                break
            request_line, headers = parsed
            method, target = request_line.split(" ", 1)
            path, _, query = target.partition("?")
            qs = {}
            for pair in query.split("&"):
                if not pair:
                    continue
                key, _, value = pair.partition("=")
                qs[key] = value
            length = int(headers.get("content-length", "0") or 0)
            body = await reader.readexactly(length) if length else b""
            delay = 0.0
            if "x-reply-delay" in headers:
                try:
                    delay = max(0.0, float(headers["x-reply-delay"]))
                except ValueError:
                    delay = 0.0
            split_body = 0.0
            if "x-split-body" in headers:
                try:
                    split_body = max(0.0, float(headers["x-split-body"]))
                except ValueError:
                    split_body = 0.0
            state.requests += 1
            state.conn_requests[conn] = state.conn_requests.get(conn, 0) + 1
            seq = state.requests
            entry = {
                "conn": conn,
                "seq": seq,
                "method": method,
                "path": path,
                "req": None,
                "value": None,
                "body": None,
                "ts": round(time.time() - state.started, 6),
            }

            if method == "GET" and path == "/warm":
                entry["kind"] = "warm"
                state.append(entry)
                await reply(writer, 200, {"ok": True, "conn": conn}, delay=delay, split_body=split_body, conn=conn)
                continue
            if method == "GET" and path == "/__health__":
                entry["kind"] = "health"
                state.append(entry)
                await reply(
                    writer,
                    200,
                    {"ok": True, "requests": state.requests, "conn": conn},
                    delay=delay,
                    split_body=split_body,
                    conn=conn,
                )
                continue
            if method == "GET" and path == "/__observe__":
                doc_id = qs.get("doc", "")
                entry["kind"] = "observe"
                entry["doc_id"] = doc_id
                # Snapshot before appending the observe request itself so the
                # observer call is not part of its own view.
                requests = [
                    row
                    for row in state.log
                    if row.get("kind") in ("post", "get") and row.get("doc") == doc_id
                ]
                state.append(entry)
                await reply(
                    writer,
                    200,
                    {
                        "doc": state.docs.get(doc_id),
                        "requests": requests,
                        "connections": len({row["conn"] for row in requests}),
                        "conn_requests": {
                            str(row["conn"]): state.conn_requests.get(row["conn"], 0)
                            for row in requests
                        },
                        "conn": conn,
                    },
                    delay=delay,
                    split_body=split_body,
                    conn=conn,
                )
                continue
            if path.startswith("/doc/"):
                doc_id = path[len("/doc/") :]
                if method == "POST":
                    try:
                        payload = json.loads(body or b"{}")
                    except json.JSONDecodeError:
                        await reply(writer, 400, {"error": "bad json"}, delay=delay, split_body=split_body, conn=conn)
                        continue
                    value = payload.get("value")
                    req = payload.get("req")
                    state.docs[doc_id] = value
                    entry.update(kind="post", doc=doc_id, req=req, value=value, body=body.decode("utf-8", "replace"))
                    state.append(entry)
                    await reply(
                        writer,
                        200,
                        {"req": req, "stored": value, "conn": conn},
                        delay=delay,
                        split_body=split_body,
                        conn=conn,
                    )
                    continue
                if method == "GET":
                    req = qs.get("req")
                    entry.update(kind="get", doc=doc_id, req=req, value=state.docs.get(doc_id))
                    state.append(entry)
                    await reply(
                        writer,
                        200,
                        {"req": req, "value": state.docs.get(doc_id), "conn": conn},
                        delay=delay,
                        split_body=split_body,
                        conn=conn,
                    )
                    continue
            entry["kind"] = "unhandled"
            state.append(entry)
            await reply(writer, 404, {"error": "not found", "conn": conn}, delay=delay, split_body=split_body, conn=conn)
    except (ConnectionResetError, asyncio.IncompleteReadError, BrokenPipeError):
        pass
    except asyncio.CancelledError:
        raise
    finally:
        writer.close()
        with __import__("contextlib").suppress(BaseException):
            await writer.wait_closed()


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port-file", required=True)
    parser.add_argument("--log-file", default=None)
    args = parser.parse_args()

    state = State()
    if args.log_file:
        state.log_path = Path(args.log_file)
        state.log_path.write_text("", encoding="utf-8")

    server = await asyncio.start_server(
        lambda r, w: handle(r, w, state), "127.0.0.1", 0, backlog=64
    )
    assert server.sockets
    port = server.sockets[0].getsockname()[1]
    Path(args.port_file).write_text(str(port), encoding="utf-8")
    print(f"LISTEN 127.0.0.1:{port}", flush=True)

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    async with server:
        await stop.wait()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(130)
