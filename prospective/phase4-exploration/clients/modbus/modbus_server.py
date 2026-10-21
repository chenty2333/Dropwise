#!/usr/bin/env python3
"""Standalone asyncio Modbus TCP server for the round-3 tokio-modbus scenario.

Public MBAP framing only (no library): 7-byte MBAP header (transaction id,
protocol id 0, length, unit id) + PDU.  Functions 3 (read holding registers),
6 (write single register) and 16 (write multiple registers); other functions get
an illegal-function exception.

The server owns four holding registers seeded to [100, 0, 0, 0] and appends one
JSON line per request to --log so the raw server-side view (what arrived, what
it applied, when) is independent of the Rust client under test.

--delay-ms postpones every reply after the request has been applied.  The
round-3 experiment uses 0 here and puts the pending mechanism in
dropwise::net::Proxy (reply_delay=50ms) instead, so there is one mechanism.

Usage: python3 modbus_server.py --port 0 --delay-ms 0 --log FILE
Prints "PORT=<port>" then "READY" on stdout.
"""
import argparse
import asyncio
import json
import struct
import time

PROTOCOL_ID = 0
SEED = [100, 0, 0, 0]


class State:
    def __init__(self, log_path, delay_ms):
        self.regs = list(SEED)
        self.log_path = log_path
        self.delay = delay_ms / 1000.0
        self.seq = 0

    def log(self, rec):
        rec["seq"] = self.seq
        rec["t"] = round(time.monotonic(), 6)
        self.seq += 1
        with open(self.log_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, sort_keys=True) + "\n")


def exception_pdu(func, code):
    return bytes([func | 0x80, code])


def pdu_for(state, unit, func, body, txid):
    rec = {"txid": txid, "unit": unit, "func": func}
    if func == 3:
        if len(body) != 4:
            state.log(dict(rec, result="bad-length"))
            return exception_pdu(func, 3)
        addr, qty = struct.unpack(">HH", body)
        rec.update(addr=addr, qty=qty)
        if qty < 1 or qty > 125 or addr + qty > len(state.regs):
            state.log(dict(rec, result="illegal-address"))
            return exception_pdu(func, 2)
        values = state.regs[addr:addr + qty]
        rec.update(values=values, result="ok")
        state.log(rec)
        return bytes([func, 2 * qty]) + struct.pack(">%dH" % qty, *values)
    if func == 6:
        if len(body) != 4:
            state.log(dict(rec, result="bad-length"))
            return exception_pdu(func, 3)
        addr, value = struct.unpack(">HH", body)
        rec.update(addr=addr, value=value)
        if addr >= len(state.regs):
            state.log(dict(rec, result="illegal-address"))
            return exception_pdu(func, 2)
        state.regs[addr] = value
        rec.update(result="ok", applied=value)
        state.log(rec)
        return bytes([func]) + struct.pack(">HH", addr, value)
    if func == 16:
        if len(body) < 5:
            state.log(dict(rec, result="bad-length"))
            return exception_pdu(func, 3)
        addr, qty, nbytes = struct.unpack(">HHB", body[:5])
        rec.update(addr=addr, qty=qty, nbytes=nbytes)
        if qty < 1 or qty > 123 or nbytes != 2 * qty or len(body) != 5 + nbytes:
            state.log(dict(rec, result="illegal-value"))
            return exception_pdu(func, 3)
        if addr + qty > len(state.regs):
            state.log(dict(rec, result="illegal-address"))
            return exception_pdu(func, 2)
        values = list(struct.unpack(">%dH" % qty, body[5:]))
        state.regs[addr:addr + qty] = values
        rec.update(values=values, result="ok")
        state.log(rec)
        return bytes([func]) + struct.pack(">HH", addr, qty)
    state.log(dict(rec, result="illegal-function"))
    return exception_pdu(func, 1)


async def handle(reader, writer, state):
    peer = writer.get_extra_info("peername")
    try:
        while True:
            head = await reader.readexactly(7)
            txid, proto, length, unit = struct.unpack(">HHHB", head)
            body = await reader.readexactly(length - 1)
            if proto != PROTOCOL_ID or length < 2:
                state.log({"txid": txid, "unit": unit, "func": None,
                           "result": "bad-mbap", "protocol_id": proto,
                           "length": length})
                return
            func = body[0]
            pdu = pdu_for(state, unit, func, body[1:], txid)
            if state.delay:
                await asyncio.sleep(state.delay)
            out = struct.pack(">HHHB", txid, PROTOCOL_ID, len(pdu) + 1, unit) + pdu
            writer.write(out)
            await writer.drain()
    except (asyncio.IncompleteReadError, ConnectionError):
        pass
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass
        state.log({"peer": str(peer), "result": "closed"})


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--delay-ms", type=float, default=0)
    ap.add_argument("--log", required=True)
    args = ap.parse_args()
    state = State(args.log, args.delay_ms)
    server = await asyncio.start_server(
        lambda r, w: handle(r, w, state), "127.0.0.1", args.port)
    port = server.sockets[0].getsockname()[1]
    print(f"PORT={port}", flush=True)
    print("READY", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
