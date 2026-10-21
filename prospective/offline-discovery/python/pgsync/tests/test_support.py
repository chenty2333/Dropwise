"""No-service tests for the sync-interruption harness support code."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

from pgsync.proxy import StreamParser  # noqa: E402
from pgsync.support import classify, rows_set, sanitize  # noqa: E402


class FakeLog:
    def __init__(self):
        self.records = []

    def write(self, direction, kind, **fields):
        self.records.append({"dir": direction, "type": kind, **fields})


def msg(mtype: bytes, payload: bytes) -> bytes:
    return mtype + (len(payload) + 4).to_bytes(4, "big") + payload


def startup_packet() -> bytes:
    body = (196608).to_bytes(4, "big") + b"user\x00postgres\x00database\x00dw\x00\x00"
    return (len(body) + 4).to_bytes(4, "big") + body


class ProxyParserTests(unittest.TestCase):
    def setUp(self):
        self.log = FakeLog()
        self.c2s = StreamParser("c2s", self.log)
        self.s2c = StreamParser("s2c", self.log)

    def test_startup_then_query(self):
        self.c2s.feed(startup_packet(), 0.0)
        self.assertEqual(self.log.records[0]["type"], "Startup")
        self.assertEqual(self.log.records[0]["protocol"], 196608)
        self.c2s.feed(msg(b"Q", b"BEGIN\x00"), 0.1)
        self.assertEqual(self.log.records[-1]["type"], "Q")
        self.assertEqual(self.log.records[-1]["text"], "BEGIN")

    def test_ssl_request_then_cancel_request(self):
        self.c2s.feed((8).to_bytes(4, "big") + (80877103).to_bytes(4, "big"), 0.0)
        self.assertEqual(self.log.records[-1]["type"], "SSLRequest")
        self.c2s.feed((16).to_bytes(4, "big") + (80877102).to_bytes(4, "big")
                      + (123).to_bytes(4, "big") + (456).to_bytes(4, "big"), 0.1)
        self.assertEqual(self.log.records[-1]["type"], "CancelRequest")

    def test_backend_frames_and_partial_feed(self):
        raw = msg(b"Z", b"I") + msg(b"C", b"INSERT 0 1\x00")
        for i in range(0, len(raw), 3):
            self.s2c.feed(raw[i:i + 3], 0.0)
        types = [r["type"] for r in self.log.records]
        self.assertEqual(types, ["Z", "C"])
        self.assertEqual(self.log.records[0]["status"], "I")
        self.assertEqual(self.log.records[1]["tag"], "INSERT 0 1")

    def test_query_payload_split_across_feeds(self):
        raw = msg(b"Q", b"SELECT 1\x00")
        self.c2s.feed(startup_packet(), 0.0)
        self.c2s.feed(raw[:3], 0.1)
        self.c2s.feed(raw[3:], 0.2)
        self.assertEqual(self.log.records[-1]["text"], "SELECT 1")


class ObservationTests(unittest.TestCase):
    def test_sanitize_ports_and_tables(self):
        value = {"error": "connection to 127.0.0.1:32905 failed",
                 "table": "dw_od123_4",
                 "rows": ["target=1"]}
        clean = sanitize(value)
        self.assertIn("127.0.0.1:<port>", clean["error"])
        self.assertEqual(clean["table"], "<noise>od123_4")
        self.assertEqual(clean["rows"], ["target=1"])

    def test_classify_states(self):
        self.assertEqual(classify({"tx": "IDLE", "num_transactions": 0,
                                   "closed": False}, None), "clean")
        self.assertEqual(classify({"tx": "INTRANS", "num_transactions": 1,
                                   "closed": False}, None), "wait_intrans")
        self.assertEqual(classify({"tx": "INTRANS", "num_transactions": 0,
                                   "closed": False}, None),
                         "intrans_no_local_owner")
        self.assertEqual(classify({"tx": "IDLE", "num_transactions": 1,
                                   "closed": False}, None), "pre_send_local_ahead")
        self.assertEqual(classify({"tx": "ACTIVE", "num_transactions": 1,
                                   "closed": False}, None), "unread_reply")
        self.assertEqual(classify({"closed": True}, None), "closed")

    def test_rows_set(self):
        self.assertEqual(rows_set([["target", 1], ["retry", 5]]),
                         {"target=1", "retry=5"})


if __name__ == "__main__":
    unittest.main()
