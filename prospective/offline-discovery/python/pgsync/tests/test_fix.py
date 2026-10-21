"""Lock-publication regression for the pinned synchronous repair prototype."""
import threading
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import psycopg
from psycopg import pq
from pgsync import fix


class PublicationLock:
    """Observe exactly what a waiting thread sees at the first lock release."""
    def __init__(self, conn):
        self.lock = threading.Lock()
        self.conn = conn
        self.released = []

    def __enter__(self):
        assert self.lock.acquire(blocking=False), "recursive lock acquisition"

    def __exit__(self, *args):
        self.released.append((self.conn.dirty, self.conn.closed))
        self.lock.release()


class RepairLockTests(unittest.TestCase):
    def setUp(self):
        fix.install()
        self.addCleanup(fix.uninstall)

    def run_interruption(self, method, repair_fails=False):
        original = KeyboardInterrupt("original interruption")
        conn = SimpleNamespace(dirty=False, closed=False)
        conn.lock = PublicationLock(conn)
        conn.close = lambda: setattr(conn, "closed", True)

        def wait(gen):
            self.assertTrue(conn.lock.lock.locked())
            conn.dirty = True
            raise original

        def repair(c, tx, exc):
            self.assertTrue(c.lock.lock.locked())
            self.assertIs(exc, original)
            if repair_fails:
                raise KeyboardInterrupt("second interruption")
            c.dirty = False
            return "repaired"

        conn.wait = wait
        tx = SimpleNamespace(_conn=conn, pgconn=SimpleNamespace(status=pq.ConnStatus.OK),
                             _enter_gen=lambda: None, _exit_gen=lambda *args: None)
        with patch.object(fix, "_repair_" + method, repair):
            with self.assertRaises(KeyboardInterrupt) as caught:
                if method == "entry":
                    psycopg.Transaction.__enter__(tx)
                else:
                    psycopg.Transaction.__exit__(tx, None, None, None)
        self.assertIs(caught.exception, original)
        self.assertEqual(conn.lock.released, [(repair_fails, repair_fails)])

    def test_entry_repaired_before_first_unlock(self):
        self.run_interruption("entry")

    def test_exit_repaired_before_first_unlock(self):
        self.run_interruption("exit")

    def test_second_interruption_closes_before_unlock(self):
        for method in ("entry", "exit"):
            with self.subTest(method=method):
                self.run_interruption(method, repair_fails=True)

    def test_normal_methods_keep_results(self):
        conn = SimpleNamespace(dirty=False, closed=False, wait=lambda gen: gen)
        conn.lock = PublicationLock(conn)
        tx = SimpleNamespace(_conn=conn, pgconn=SimpleNamespace(status=pq.ConnStatus.OK),
                             _enter_gen=lambda: None, _exit_gen=lambda *args: True)
        self.assertIs(psycopg.Transaction.__enter__(tx), tx)
        self.assertTrue(psycopg.Transaction.__exit__(tx, None, None, None))
        self.assertEqual(conn.lock.released, [(False, False), (False, False)])
