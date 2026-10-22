"""No-service tests for the round-5 successor probe, delivery evaluator and labels."""
from __future__ import annotations
import unittest
from pool_handoff.delivery_case import evaluate
from successor_probe.probe import ProbeResult, ProbeStatus, _is_config_error, compare

def pr(status: ProbeStatus | str, *, anomaly=None, applicable=None) -> ProbeResult:
    status = status.value if isinstance(status, ProbeStatus) else status
    if anomaly is None:
        anomaly = status in {s.value for s in (
            ProbeStatus.OPEN_TRANSACTION, ProbeStatus.CLOSED,
            ProbeStatus.ECHO_MISMATCH, ProbeStatus.METADATA_MISMATCH,
            ProbeStatus.PROTOCOL_ERROR, ProbeStatus.TRANSACTION_API_ERROR,
            ProbeStatus.INVISIBLE_WRITE,
            ProbeStatus.WRITE_ONLY_VISIBLE_AFTER_SETTLE)}
    if applicable is None:
        applicable = status not in {
            ProbeStatus.PROBE_TIMEOUT.value, ProbeStatus.BLOCKED_BY_SESSION.value,
            ProbeStatus.OPEN_TRANSACTION_ALLOWED.value}
    return ProbeResult("old", status, bool(anomaly), bool(applicable), {})

class CompareTest(unittest.TestCase):
    def test_clean_pair(self):
        self.assertEqual(compare(pr(ProbeStatus.CLEAN), pr(ProbeStatus.CLEAN))["label"],
                         "consistent_clean")

    def test_entry_contamination(self):
        c = compare(pr(ProbeStatus.OPEN_TRANSACTION), pr(ProbeStatus.CLEAN))
        self.assertEqual(c["label"], "old_anomaly")
        self.assertTrue(c["old_anomaly"])

    def test_fail_closed_is_not_contamination(self):
        self.assertEqual(compare(pr(ProbeStatus.CLOSED), pr(ProbeStatus.CLEAN))["label"],
                         "old_closed")

    def test_legitimate_outer_transaction(self):
        self.assertEqual(compare(pr(ProbeStatus.OPEN_TRANSACTION_ALLOWED),
                                 pr(ProbeStatus.CLEAN))["label"],
                         "legitimate_open_transaction")

    def test_blocked_and_timeout_are_not_applicable(self):
        for s in (ProbeStatus.BLOCKED_BY_SESSION, ProbeStatus.PROBE_TIMEOUT):
            self.assertEqual(compare(pr(s), pr(ProbeStatus.CLEAN))["label"],
                             "old_not_applicable")

    def test_reference_anomaly_is_not_old_evidence(self):
        self.assertEqual(compare(pr(ProbeStatus.CLEAN), pr(ProbeStatus.OPEN_TRANSACTION))["label"],
                         "reference_anomaly")

    def test_config_error_does_not_include_aborted_transaction(self):
        self.assertTrue(_is_config_error(Exception("read-only transaction")))
        self.assertTrue(_is_config_error(Exception("statement timeout")))
        self.assertFalse(_is_config_error(Exception("current transaction is aborted")))

class DeliveryEvaluateTest(unittest.TestCase):
    def base(self, driver="psycopg"):
        stats = ({"stats": {"pool_size": 2, "pool_available": 2, "requests_waiting": 0}}
                 if driver == "psycopg" else {"size": 2, "idle": 2})
        return {
            "driver": driver, "shape": "delivery", "cut_realized": True,
            "after_settle": {
                "legit": {"alive": True, "same_pid": True, "guc": "keeper", "temp": 7},
                "next": {"ok": True, "query": 1},
            },
            "next": {"ok": True, "query": 1},
            "final": {"stats": stats, "server": {"count": 2}},
        }

    def test_clean_psycopg_delivery(self):
        self.assertEqual(evaluate(self.base())["issues"], [])

    def test_asyncpg_reconnect_after_delivery_is_clean(self):
        rec = self.base("asyncpg")
        rec["next"]["same_as_delivered"] = False
        self.assertTrue(evaluate(rec)["clean"])

    def test_unrealized_delivery_cut(self):
        rec = self.base(); rec["cut_realized"] = False
        self.assertIn("delivery_cut_not_realized", evaluate(rec)["issues"])

    def test_legit_user_lost(self):
        rec = self.base(); rec["after_settle"]["legit"]["alive"] = False
        self.assertIn("legit_user_lost", evaluate(rec)["issues"])

    def test_capacity_not_recovered(self):
        rec = self.base(); rec["final"]["stats"]["stats"]["pool_available"] = 1
        self.assertIn("final_available_1", evaluate(rec)["issues"])

if __name__ == "__main__":
    unittest.main()
