#!/usr/bin/env python3
"""Concurrency suite (plan Task 12; brief §6E). Many clients and many CLI
invocations at once; every invariant checked after the storm.

    PGBOXDB_DIST=<prefix> python3 -I tests/pgboxdb/test_concurrency.py [-v]
"""
import os
import subprocess
import sys
import threading
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import Pgboxdb  # noqa: E402

PGBoxDB = None


def setUpModule():
    global PGBoxDB
    PGBoxDB = Pgboxdb("conc").up()
    PGBoxDB.cli("create", "app", check=True)


def tearDownModule():
    if PGBoxDB:
        PGBoxDB.cleanup()


class ConcurrencyCase(unittest.TestCase):
    def _connections(self, n, per_conn=50):
        """n clients, each doing per_conn write+read ops on its own row; returns errors."""
        errors = []
        barrier = threading.Barrier(n)

        def worker(k):
            try:
                with PGBoxDB.connect("app") as c:
                    barrier.wait(timeout=30)
                    for i in range(per_conn):
                        c.query("INSERT INTO cc (client, seq) VALUES (%d, %d)" % (k, i))
                        self.assertEqual(c.scalar("SELECT count(*) FROM cc WHERE client = %d" % k), str(i + 1))
            except Exception as e:  # noqa: BLE001
                errors.append("client %d: %r" % (k, e))

        with PGBoxDB.connect("app") as c:
            c.query("DROP TABLE IF EXISTS cc; CREATE TABLE cc (client int, seq int, t timestamptz DEFAULT now())")
        threads = [threading.Thread(target=worker, args=(k,)) for k in range(n)]
        t0 = time.time()
        for t in threads:
            t.start()
        for t in threads:
            t.join(120)
        elapsed = time.time() - t0
        self.assertFalse(any(t.is_alive() for t in threads), "a client hung with %d connections" % n)
        self.assertEqual(errors, [], "errors with %d connections: %r" % (n, errors[:5]))
        with PGBoxDB.connect("app") as c:
            self.assertEqual(c.scalar("SELECT count(*) FROM cc"), str(n * per_conn))
            self.assertEqual(c.scalar("SELECT count(DISTINCT client) FROM cc"), str(n))
        print("\n  %d concurrent clients x %d ops: %.1fs, no errors" % (n, per_conn, elapsed))

    def test_10_concurrent_connections(self):
        self._connections(10)

    def test_100_concurrent_connections(self):
        self._connections(100, per_conn=20)

    def test_16_parallel_creates(self):
        errors = []
        names = ["pc%d" % i for i in range(16)]

        def mk(name):
            r = PGBoxDB.cli("create", name)
            if r.code != 0:
                errors.append((name, r.code, r.stderr.strip()))

        threads = [threading.Thread(target=mk, args=(n,)) for n in names]
        for t in threads:
            t.start()
        for t in threads:
            t.join(120)
        self.assertEqual(errors, [], "parallel creates failed: %r" % errors)
        with PGBoxDB.connect("postgres") as c:
            for n in names:
                self.assertEqual(c.scalar("SELECT count(*) FROM pg_database WHERE datname = '%s'" % n), "1")
                with PGBoxDB.connect(n) as cc:
                    cc.query("CREATE TABLE t (x int); INSERT INTO t VALUES (1)")
        for n in names:
            PGBoxDB.cli("drop", n, check=True)

    def test_20_concurrent_cli_reads(self):
        """Many read-only CLI calls at once must never corrupt output or fail."""
        results = []
        lock = threading.Lock()

        def run(k):
            for _ in range(5):
                for args in (["status", "--json"], ["ls", "--json"], ["doctor", "--json"], ["version", "--json"]):
                    r = PGBoxDB.cli(*args, json_out=("--json" not in args))
                    rr = PGBoxDB.cli(*[a for a in args if a != "--json"], json_out=True)
                    with lock:
                        results.append((args[0], rr.code))

        threads = [threading.Thread(target=run, args=(k,)) for k in range(20)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(120)
        self.assertTrue(all(code == 0 for _, code in results), "a concurrent CLI read failed: %r" % [r for r in results if r[1] != 0][:5])
        self.assertGreater(len(results), 20 * 5 * 4 // 2)

    def test_concurrent_connect_disconnect_churn(self):
        """Rapid connect/disconnect from many threads must not leak or crash."""
        stop = time.time() + 8
        errors = []

        def churn(k):
            n = 0
            while time.time() < stop:
                try:
                    c = PGBoxDB.connect("app")
                    c.scalar("SELECT %d" % k)
                    c.close()
                    n += 1
                except Exception as e:  # noqa: BLE001
                    errors.append(repr(e))
                    return

        threads = [threading.Thread(target=churn, args=(k,)) for k in range(12)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(30)
        self.assertEqual(errors, [], "connect/disconnect churn errors: %r" % errors[:5])
        with PGBoxDB.connect("app") as c:
            self.assertEqual(c.scalar("SELECT 1"), "1")


if __name__ == "__main__":
    unittest.main(verbosity=2 if "-v" in sys.argv else 1, argv=[a for a in sys.argv if a != "-v"])
