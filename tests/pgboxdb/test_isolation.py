#!/usr/bin/env python3
"""Transaction isolation, locking and timeout suite (plan Task 11; brief §6E).

    PGBOXDB_DIST=<prefix> python3 -I tests/pgboxdb/test_isolation.py [-v]

Every case uses two or more real sessions; every wait runs on a thread with
a deadline so a hang fails the test instead of the suite.
"""
import os
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
    PGBoxDB = Pgboxdb("iso").up()
    PGBoxDB.cli("create", "iso", check=True)


def tearDownModule():
    if PGBoxDB:
        PGBoxDB.cleanup()


def run_with_deadline(fn, seconds):
    """Runs fn on a thread; returns ('ok', value) | ('error', DbError) | ('timeout', None)."""
    box = {}

    def target():
        try:
            box["r"] = ("ok", fn())
        except pgclient.DbError as e:
            box["r"] = ("error", e)
        except Exception as e:  # noqa: BLE001
            box["r"] = ("exception", e)

    th = threading.Thread(target=target, daemon=True)
    th.start()
    th.join(seconds)
    if th.is_alive():
        return ("timeout", None, th)
    return box["r"] + (th,)


class IsoCase(unittest.TestCase):
    def setUp(self):
        self.a = PGBoxDB.connect("iso")
        self.b = PGBoxDB.connect("iso")
        self.tables = []

    def tearDown(self):
        for c in (self.a, self.b):
            try:
                c.close()
            except Exception:  # noqa: BLE001
                pass
        with PGBoxDB.connect("iso") as c:
            for t in self.tables:
                c.query("DROP TABLE IF EXISTS %s" % t)

    def table(self, name, ddl):
        full = "iso_%s_%s" % (self._testMethodName[:20], name)
        self.tables.append(full)
        self.a.query("DROP TABLE IF EXISTS %s; CREATE TABLE %s %s" % (full, full, ddl))
        return full

    def test_default_isolation_is_read_committed(self):
        self.assertEqual(self.a.scalar("SHOW default_transaction_isolation"), "read committed")
        self.a.query("BEGIN")
        self.assertEqual(self.a.scalar("SELECT current_setting('transaction_isolation')"), "read committed")
        self.a.query("COMMIT")

    def test_repeatable_read_prevents_phantoms(self):
        t = self.table("ph", "(id int PRIMARY KEY)")
        self.a.query("INSERT INTO %s VALUES (1), (2)" % t)
        self.a.query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        self.assertEqual(self.a.scalar("SELECT count(*) FROM %s" % t), "2")
        self.b.query("INSERT INTO %s VALUES (3)" % t)
        self.assertEqual(self.a.scalar("SELECT count(*) FROM %s" % t), "2", "phantom row visible in REPEATABLE READ")
        self.a.query("COMMIT")
        self.assertEqual(self.a.scalar("SELECT count(*) FROM %s" % t), "3")

    def test_serializable_detects_write_skew(self):
        t = self.table("ws", "(id int PRIMARY KEY, v int)")
        self.a.query("INSERT INTO %s VALUES (1, 10), (2, 10)" % t)
        self.a.query("BEGIN ISOLATION LEVEL SERIALIZABLE")
        self.b.query("BEGIN ISOLATION LEVEL SERIALIZABLE")
        self.a.scalar("SELECT sum(v) FROM %s" % t)
        self.b.scalar("SELECT sum(v) FROM %s" % t)
        self.a.query("UPDATE %s SET v = v - 15 WHERE id = 1" % t)
        self.b.query("UPDATE %s SET v = v - 15 WHERE id = 2" % t)
        self.a.query("COMMIT")
        with self.assertRaises(pgclient.DbError) as cm:
            self.b.query("COMMIT")
        self.assertEqual(cm.exception.sqlstate, "40001", "write skew must fail with serialization_failure")
        self.assertEqual(self.a.scalar("SELECT sum(v) FROM %s" % t), "5", "exactly one of the two updates committed")

    def test_deadlock_is_detected(self):
        t = self.table("dl", "(id int PRIMARY KEY, v int)")
        self.a.query("INSERT INTO %s VALUES (1, 0), (2, 0)" % t)
        self.a.query("BEGIN; UPDATE %s SET v = 1 WHERE id = 1" % t)
        self.b.query("BEGIN; UPDATE %s SET v = 2 WHERE id = 2" % t)
        # Each crossing update runs on its own thread. The deadlock detector
        # cancels one with 40P01; that session must then ROLLBACK to release its
        # locks so the survivor can finish. Roll back inside the thread.
        seen = {}

        def cross(conn, name, row):
            try:
                conn.query("UPDATE %s SET v = 9 WHERE id = %d" % (t, row))
                seen[name] = "ok"
            except pgclient.DbError as e:
                seen[name] = e.sqlstate
                try:
                    conn.query("ROLLBACK")
                except pgclient.DbError:
                    pass

        ta = threading.Thread(target=cross, args=(self.a, "a", 2))
        tb = threading.Thread(target=cross, args=(self.b, "b", 1))
        ta.start(); tb.start()
        ta.join(30); tb.join(30)
        self.assertFalse(ta.is_alive() or tb.is_alive(), "deadlock was not resolved within 30s (outcomes so far: %r)" % seen)
        states = sorted(seen.values())
        self.assertEqual(states.count("40P01"), 1, "exactly one deadlock victim expected, got %r" % seen)
        self.assertEqual(states.count("ok"), 1, "the survivor should have committed its update, got %r" % seen)
        for c in (self.a, self.b):
            try:
                c.query("ROLLBACK")
            except pgclient.DbError:
                pass

    def test_lock_timeout_nowait_skip_locked(self):
        t = self.table("lt", "(id int PRIMARY KEY, v int)")
        self.a.query("INSERT INTO %s VALUES (1, 0), (2, 0), (3, 0)" % t)
        self.a.query("BEGIN; SELECT * FROM %s WHERE id = 1 FOR UPDATE" % t)
        self.b.query("SET lock_timeout = '300ms'")
        t0 = time.time()
        with self.assertRaises(pgclient.DbError) as cm:
            self.b.query("UPDATE %s SET v = 9 WHERE id = 1" % t)
        self.assertEqual(cm.exception.sqlstate, "55P03")
        self.assertLess(time.time() - t0, 5, "lock_timeout took too long to fire")
        with self.assertRaises(pgclient.DbError) as cm:
            self.b.query("SELECT * FROM %s WHERE id = 1 FOR UPDATE NOWAIT" % t)
        self.assertEqual(cm.exception.sqlstate, "55P03")
        rows = self.b.query("SELECT id FROM %s ORDER BY id FOR UPDATE SKIP LOCKED" % t).rows
        self.assertEqual(rows, [["2"], ["3"]])
        self.a.query("COMMIT")
        self.assertEqual(self.b.scalar("SELECT v FROM %s WHERE id = 1" % t), "0")

    def test_statement_timeout(self):
        self.a.query("SET statement_timeout = '200ms'")
        t0 = time.time()
        with self.assertRaises(pgclient.DbError) as cm:
            self.a.query("SELECT pg_sleep(5)")
        self.assertEqual(cm.exception.sqlstate, "57014")
        self.assertLess(time.time() - t0, 4)
        self.assertEqual(self.a.scalar("SELECT 'alive'"), "alive")

    def test_idle_in_transaction_timeout(self):
        self.a.query("SET idle_in_transaction_session_timeout = '300ms'")
        self.a.query("BEGIN; SELECT 1")
        time.sleep(1.0)
        with self.assertRaises((pgclient.DbError, pgclient.ConnectionClosed)) as cm:
            self.a.query("SELECT 1")
        if isinstance(cm.exception, pgclient.DbError):
            self.assertEqual(cm.exception.sqlstate, "25P03")

    def test_long_transaction_does_not_block_unrelated_writers(self):
        t1 = self.table("lt1", "(id int PRIMARY KEY, v int)")
        t2 = self.table("lt2", "(id int PRIMARY KEY, v int)")
        self.a.query("INSERT INTO %s VALUES (1, 0); INSERT INTO %s VALUES (1, 0)" % (t1, t2))
        self.a.query("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM %s; UPDATE %s SET v = 1 WHERE id = 1" % (t1, t1))
        r = run_with_deadline(lambda: self.b.query("UPDATE %s SET v = 5 WHERE id = 1" % t2), 10)
        self.assertEqual(r[0], "ok", "writer on an unrelated table was blocked: %r" % (r[:2],))
        r = run_with_deadline(lambda: self.b.query("INSERT INTO %s VALUES (2, 0)" % t1), 10)
        self.assertEqual(r[0], "ok", "insert into the same table (different row) was blocked")
        self.a.query("COMMIT")
        self.assertEqual(self.b.scalar("SELECT count(*) FROM %s" % t1), "2")

    def test_for_share_is_compatible_for_update_is_not(self):
        t = self.table("fs", "(id int PRIMARY KEY)")
        self.a.query("INSERT INTO %s VALUES (1)" % t)
        self.a.query("BEGIN; SELECT * FROM %s FOR SHARE" % t)
        r = run_with_deadline(lambda: self.b.query("BEGIN; SELECT * FROM %s FOR SHARE" % t), 5)
        self.assertEqual(r[0], "ok")
        self.b.query("COMMIT")
        r = run_with_deadline(lambda: self.b.query("SELECT * FROM %s FOR UPDATE NOWAIT" % t), 5)
        self.assertEqual(r[0], "error")
        self.assertEqual(r[1].sqlstate, "55P03")
        self.a.query("COMMIT")

    def test_advisory_locks(self):
        self.assertEqual(self.a.scalar("SELECT pg_try_advisory_lock(4242)"), "t")
        self.assertEqual(self.b.scalar("SELECT pg_try_advisory_lock(4242)"), "f")
        self.assertEqual(self.a.scalar("SELECT pg_advisory_unlock(4242)"), "t")
        self.assertEqual(self.b.scalar("SELECT pg_try_advisory_lock(4242)"), "t")
        self.b.query("SELECT pg_advisory_unlock_all()")
        self.a.query("BEGIN; SELECT pg_advisory_xact_lock(7)")
        r = run_with_deadline(lambda: self.b.query("SELECT pg_advisory_xact_lock(7)"), 1.0)
        self.assertEqual(r[0], "timeout", "xact advisory lock should block the second session")
        self.a.query("COMMIT")
        r[2].join(10)
        self.assertFalse(r[2].is_alive())
        self.b.query("ROLLBACK")

    def test_drop_force_terminates_sessions_with_57P01(self):
        PGBoxDB.cli("create", "victim", check=True)
        v = PGBoxDB.connect("victim")
        v.query("CREATE TABLE x (i int); BEGIN; INSERT INTO x VALUES (1)")
        r = PGBoxDB.cli("drop", "victim", "--force")
        self.assertEqual(r.code, 0, repr(r))
        with self.assertRaises((pgclient.DbError, pgclient.ConnectionClosed)) as cm:
            v.query("SELECT 1")
        if isinstance(cm.exception, pgclient.DbError):
            self.assertEqual(cm.exception.sqlstate, "57P01")
        self.assertEqual(self.a.scalar("SELECT count(*) FROM pg_database WHERE datname = 'victim'"), "0")

    def test_concurrent_create_drop_same_name(self):
        """Many sessions race to CREATE and DROP one name: no partial dir, consistent catalog."""
        results = []
        lock = threading.Lock()

        def racer(k):
            with PGBoxDB.connect("postgres") as c:
                for i in range(6):
                    for sql in ('CREATE DATABASE race_db', 'DROP DATABASE race_db'):
                        try:
                            c.query(sql)
                            outcome = "ok"
                        except pgclient.DbError as e:
                            outcome = e.sqlstate
                        with lock:
                            results.append((k, sql[:6], outcome))

        threads = [threading.Thread(target=racer, args=(k,)) for k in range(6)]
        for th in threads:
            th.start()
        for th in threads:
            th.join(120)
        self.assertTrue(all(not th.is_alive() for th in threads), "a racer hung")
        allowed = {"ok", "42P04", "3D000", "55006", "23505", "55P03"}
        bad = [r for r in results if r[2] not in allowed]
        self.assertEqual(bad, [], "unexpected outcomes: %r" % bad[:5])
        self.assertNotIn("XX000", {r[2] for r in results}, "internal error during concurrent create/drop")
        with PGBoxDB.connect("postgres") as c:
            c.query("DROP DATABASE IF EXISTS race_db")
            known = {r[0] for r in c.query("SELECT oid::text FROM pg_database").rows}
        orphans = [d for d in PGBoxDB.base_dirs() if d.isdigit() and d not in known]
        self.assertEqual(orphans, [], "concurrent create/drop left orphan dirs")


if __name__ == "__main__":
    unittest.main(verbosity=2 if "-v" in sys.argv else 1, argv=[a for a in sys.argv if a != "-v"])
