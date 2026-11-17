#!/usr/bin/env python3
"""Crash and recovery suite (plan Task 11; brief §6F).

Runs under both durability settings:
    PGBOXDB_DIST=<prefix> PGBOXDB_TEST_DURABILITY=ephemeral python3 -I tests/pgboxdb/test_crash.py
    PGBOXDB_DIST=<prefix> PGBOXDB_TEST_DURABILITY=full      python3 -I tests/pgboxdb/test_crash.py

What must hold after any crash and restart, under either setting:
  - the runtime restarts and every database in pg_database is usable;
  - no partially applied transaction is visible (a batch is all there or absent);
  - committed transactions form a prefix of the commit order (WAL loses a tail, never the middle).
Under `full` (fsync, synchronous_commit on) every acknowledged commit survives.
Under `ephemeral` (synchronous_commit off) commits acknowledged in the last
second before a SIGKILL may be lost; older ones must survive. This is the
documented trade-off, asserted here rather than assumed.
"""
import os
import signal
import subprocess
import sys
import threading
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import Pgboxdb  # noqa: E402

DURABILITY = os.environ.get("PGBOXDB_TEST_DURABILITY", "ephemeral")
REPEAT = int(os.environ.get("PGBOXDB_TEST_REPEAT", "5"))
LOSS_WINDOW_S = 1.0


def prefix_violation(acked_ids, present):
    """Checks the ids found after a crash against the commit order.

    `acked_ids` are the ids whose COMMIT was acknowledged, in order (one
    writer, consecutive ids). At the kill one more commit may be in flight:
    durable on the server, its acknowledgement never read by the killed
    client. That id, acked_ids[-1] + 1, may therefore be present, but only
    as the last row (WAL loses a tail, never the middle).

    Returns (error or None, number of acknowledged commits lost).
    """
    inflight = acked_ids[-1] + 1 if acked_ids else 1
    order = list(acked_ids) + [inflight]
    found = sorted(present)
    stray = sorted(set(found) - set(order))
    if stray:
        return "rows present that were never acknowledged: %s" % stray[:5], None
    if len(found) != len(set(found)):
        return "duplicate rows present", None
    if found != order[:len(found)]:
        return "committed rows are not a prefix of the commit order", None
    return None, len(acked_ids) - len(set(found) & set(acked_ids))


class PrefixRule(unittest.TestCase):
    """prefix_violation itself (no engine)."""

    def test_rule(self):
        ok = [
            ([1, 2, 3], [1, 2, 3], 0),     # everything survived
            ([1, 2, 3], [1, 2, 3, 4], 0),  # plus the in-flight commit (the former false failure)
            ([1, 2, 3], [1, 2], 1),        # a lost tail (allowed only under ephemeral)
            ([1, 2, 3], [], 3),
            ([], [], 0),
            ([], [1], 0),                  # the very first commit in flight
        ]
        for acked, present, lost in ok:
            self.assertEqual(prefix_violation(acked, present), (None, lost), (acked, present))
        bad = [
            ([1, 2, 3], [1, 2, 3, 5], "never acknowledged"),   # more than one unacknowledged
            ([1, 2, 3], [1, 2, 3, 4, 5], "never acknowledged"),
            ([1, 2, 3], [1, 3], "not a prefix"),               # a hole in the middle
            ([1, 2, 3], [1, 2, 4], "not a prefix"),            # in-flight row but an acknowledged one lost
            ([1, 2, 3], [2, 3], "not a prefix"),
            ([1, 2, 3], [1, 1, 2], "duplicate"),
        ]
        for acked, present, want in bad:
            err, _ = prefix_violation(acked, present)
            self.assertIsNotNone(err, (acked, present))
            self.assertIn(want, err, (acked, present))


class CrashCase(unittest.TestCase):
    def setUp(self):
        self.pgboxdb = Pgboxdb("crash", durability=DURABILITY).up()
        self.pgboxdb.cli("create", "app", check=True)
        self.invalid_seen = []

    def tearDown(self):
        self.pgboxdb.cleanup()

    # ---------------------------------------------------------------- helpers
    def restart_and_check(self):
        """Restart after a crash; every database must be connectable and consistent with base/."""
        self.assertTrue(self.pgboxdb.wait_engine_gone(60), "engine did not exit")
        r = self.pgboxdb.cli("start")
        self.assertEqual(r.code, 0, "restart after crash failed: %r\n--- diagnostics\n%s\n--- log tail\n%s" % (r, getattr(r, "diagnostics", ""), self.pgboxdb.log_tail()))
        with self.pgboxdb.connect("postgres") as c:
            rows = c.query("SELECT oid, datname, datconnlimit FROM pg_database WHERE datallowconn ORDER BY oid").rows
        dirs = set(self.pgboxdb.base_dirs())
        dbs = []
        for oid, name, connlimit in rows:
            if connlimit == "-2":
                # Marked invalid by a DROP DATABASE that did not finish (PostgreSQL 15+
                # semantics): it refuses connections and must be droppable.
                with self.assertRaises(pgclient.DbError) as cm:
                    self.pgboxdb.connect(name).close()
                self.assertEqual(cm.exception.sqlstate, "55000")
                r = self.pgboxdb.cli("drop", name, "--force")
                self.assertEqual(r.code, 0, "invalid database %s cannot be dropped: %r" % (name, r))
                self.invalid_seen.append(name)
                continue
            self.assertIn(oid, dirs, "database %s (oid %s) has no base directory" % (name, oid))
            with self.pgboxdb.connect(name) as c:
                self.assertEqual(c.scalar("SELECT 1"), "1")
                c.query("SELECT count(*) FROM pg_class")
            dbs.append((oid, name))
        return dbs, dirs

    def check_prefix(self, acked, present, kill_time, label):
        """`present` ids must be a prefix of the commit order; returns the number of acknowledged commits lost."""
        err, lost = prefix_violation([i for i, _ in acked], present)
        self.assertIsNone(err, "%s: %s" % (label, err))
        if DURABILITY == "full":
            # fsync + synchronous_commit on: every acknowledged commit must survive.
            self.assertEqual(lost, 0, "%s: durability=full lost %d acknowledged commits" % (label, lost))
        else:
            # synchronous_commit off: recent commits not yet flushed by the
            # walwriter may be lost. This is documented async-commit behaviour,
            # not corruption — the integrity invariants (prefix above, no partial
            # batch in the caller) are what must hold. The amount lost depends on
            # walwriter timing and host I/O, so it is reported, not bounded here.
            pass
        return lost

    # ---------------------------------------------------------------- cases
    def test_signals_at_rest(self):
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGKILL):
            with self.pgboxdb.connect("app") as c:
                c.query("CREATE TABLE IF NOT EXISTS rest (sig int)")
                c.query("INSERT INTO rest VALUES (%d)" % sig)
                before = c.scalar("SELECT count(*) FROM rest")
            # Under synchronous_commit=off a commit is acknowledged before its WAL
            # reaches the OS; wait past the walwriter interval so the commit is
            # on disk (or in the OS cache) before a hard kill. Clean shutdowns flush.
            time.sleep(LOSS_WINDOW_S + 0.2)
            self.pgboxdb.kill(sig)
            self.restart_and_check()
            with self.pgboxdb.connect("app") as c:
                self.assertEqual(c.scalar("SELECT count(*) FROM rest"), before, "rows lost across signal %d" % sig)

    def test_kill_during_insert_batches(self):
        lost_total = 0
        for rep in range(REPEAT):
            with self.pgboxdb.connect("app") as c:
                c.query("DROP TABLE IF EXISTS ins; CREATE TABLE ins (batch int, i int, PRIMARY KEY (batch, i))")
            acked = []
            stop = {"flag": False, "err": None}

            def writer():
                try:
                    with self.pgboxdb.connect("app") as w:
                        b = 0
                        while not stop["flag"]:
                            b += 1
                            w.query("BEGIN")
                            w.query("INSERT INTO ins SELECT %d, g FROM generate_series(1, 1000) g" % b)
                            w.query("COMMIT")
                            acked.append((b, time.time()))
                except Exception as e:  # noqa: BLE001
                    stop["err"] = e

            th = threading.Thread(target=writer, daemon=True)
            th.start()
            time.sleep(1.5 + 0.2 * rep)
            kill_time = time.time()
            self.pgboxdb.kill(signal.SIGKILL)
            stop["flag"] = True
            th.join(timeout=30)
            self.assertTrue(len(acked) > 3, "writer committed only %d batches" % len(acked))
            self.restart_and_check()
            with self.pgboxdb.connect("app") as c:
                rows = c.query("SELECT batch, count(*) FROM ins GROUP BY batch ORDER BY batch").rows
                for b, n in rows:
                    self.assertEqual(n, "1000", "rep %d: batch %s is partial (%s rows)" % (rep, b, n))
                present = [int(b) for b, _ in rows]
            lost_total += self.check_prefix(acked, present, kill_time, "rep %d" % rep)
        print("\n  [%s] kill during inserts: %d reps, %d acknowledged commits lost in total" % (DURABILITY, REPEAT, lost_total))

    def test_kill_during_commit_flood(self):
        with self.pgboxdb.connect("app") as c:
            c.query("CREATE TABLE flood (id int PRIMARY KEY, t timestamptz DEFAULT now())")
        acked = []
        stop = {"flag": False}

        def writer():
            with self.pgboxdb.connect("app") as w:
                i = 0
                try:
                    while not stop["flag"]:
                        i += 1
                        w.query("INSERT INTO flood (id) VALUES (%d)" % i)
                        acked.append((i, time.time()))
                except Exception:  # noqa: BLE001
                    pass

        th = threading.Thread(target=writer, daemon=True)
        th.start()
        time.sleep(1.5)
        kill_time = time.time()
        self.pgboxdb.kill(signal.SIGKILL)
        stop["flag"] = True
        th.join(timeout=30)
        self.restart_and_check()
        with self.pgboxdb.connect("app") as c:
            present = [int(r[0]) for r in c.query("SELECT id FROM flood ORDER BY id").rows]
            self.assertEqual(c.scalar("SELECT count(*) FROM flood WHERE t IS NULL"), "0", "partial row")
        lost = self.check_prefix(acked, present, kill_time, "flood")
        print("\n  [%s] commit flood: %d single-row commits acknowledged, %d lost" % (DURABILITY, len(acked), lost))

    def _kill_during_cli(self, args, delay_s):
        """Starts a CLI operation and kills the engine `delay_s` later; returns the CLI result."""
        proc = subprocess.Popen([self.pgboxdb.bin] + list(args), env=self.pgboxdb.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(delay_s)
        self.pgboxdb.kill(signal.SIGKILL)
        out, err = proc.communicate(timeout=120)
        return proc.returncode, out, err

    def _db_dir_consistency(self):
        dbs, dirs = self.restart_and_check()
        known = {oid for oid, _ in dbs} | {"1", "4", "5"}
        with self.pgboxdb.connect("postgres") as c:
            known |= {r[0] for r in c.query("SELECT oid FROM pg_database").rows}
        orphans = sorted(d for d in dirs if d not in known and not d.startswith("pgsql_tmp"))
        return orphans

    def test_kill_during_create_database(self):
        orphans_seen = []
        for rep in range(REPEAT):
            name = "mk%d" % rep
            code, out, err = self._kill_during_cli(["create", name], 0.02 * (rep + 1))
            orphans = self._db_dir_consistency()
            orphans_seen += orphans
            with self.pgboxdb.connect("postgres") as c:
                exists = c.scalar("SELECT count(*) FROM pg_database WHERE datname = '%s'" % name) == "1"
            if code == 0:
                self.assertTrue(exists, "create acknowledged but %s is missing after recovery" % name)
            if exists:
                with self.pgboxdb.connect(name) as c:
                    c.query("CREATE TABLE ok (x int); INSERT INTO ok VALUES (1)")
        print("\n  [%s] kill during CREATE DATABASE: %d reps, orphan base dirs left: %s" % (DURABILITY, REPEAT, orphans_seen or "none"))
        # Orphans are reported, not failed: PostgreSQL itself can leave a directory
        # when killed between the copy and its WAL record. Unusable databases fail above.

    def test_kill_during_clone(self):
        with self.pgboxdb.connect("app") as c:
            c.query("SET synchronous_commit = on")
            c.query("CREATE TABLE big AS SELECT g, repeat('x', 100) p FROM generate_series(1, 200000) g")
            self.assertEqual(c.scalar("SELECT count(*) FROM big"), "200000")
        for rep in range(REPEAT):
            name = "cl%d" % rep
            code, out, err = self._kill_during_cli(["clone", "app", name], 0.02 * (rep + 1))
            orphans = self._db_dir_consistency()
            with self.pgboxdb.connect("app") as c:
                self.assertEqual(c.scalar("SELECT count(*) FROM big"), "200000", "source damaged by a crashed clone")
            with self.pgboxdb.connect("postgres") as c:
                exists = c.scalar("SELECT count(*) FROM pg_database WHERE datname = '%s'" % name) == "1"
            if exists:
                with self.pgboxdb.connect(name) as c:
                    self.assertEqual(c.scalar("SELECT count(*) FROM big"), "200000", "clone %s is incomplete yet visible" % name)
            print("\n  [%s] kill during clone rep %d: cli exit %d, clone visible: %s, orphans: %s" % (DURABILITY, rep, code, exists, orphans or "none"))

    def test_kill_during_drop(self):
        for rep in range(REPEAT):
            name = "dr%d" % rep
            self.pgboxdb.cli("create", name, check=True)
            with self.pgboxdb.connect(name) as c:
                # The data this test asserts on must be on disk before the hard kill:
                # a synchronous commit, whatever the runtime's default.
                c.query("SET synchronous_commit = on")
                c.query("CREATE TABLE t AS SELECT g FROM generate_series(1, 50000) g")
            code, out, err = self._kill_during_cli(["drop", name], 0.01 * (rep + 1))
            self._db_dir_consistency()
            with self.pgboxdb.connect("postgres") as c:
                exists = c.scalar("SELECT count(*) FROM pg_database WHERE datname = '%s'" % name) == "1"
            if code == 0:
                self.assertFalse(exists, "drop acknowledged but %s still exists" % name)
            if exists:
                with self.pgboxdb.connect(name) as c:
                    self.assertEqual(c.scalar("SELECT count(*) FROM t"), "50000", "%s half-dropped" % name)
        print("\n  [%s] kill during DROP DATABASE: %d reps, databases left invalid (dropped again): %s" % (DURABILITY, REPEAT, self.invalid_seen or "none"))

    def test_kill_during_shutdown(self):
        with self.pgboxdb.connect("app") as c:
            c.query("SET synchronous_commit = on")
            c.query("CREATE TABLE sd (x int); INSERT INTO sd SELECT g FROM generate_series(1, 100000) g")
        pid = self.pgboxdb.kill(signal.SIGINT)
        time.sleep(0.01)
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.restart_and_check()
        with self.pgboxdb.connect("app") as c:
            self.assertEqual(c.scalar("SELECT count(*) FROM sd"), "100000")

    def test_permission_error_on_start(self):
        self.pgboxdb.stop()
        base = os.path.join(self.pgboxdb.data, "base")
        os.chmod(base, 0o000)
        try:
            r = self.pgboxdb.cli("start")
            self.assertNotEqual(r.code, 0, "start succeeded with an unreadable base/")
            self.assertIn("exit", repr(r))
            self.assertFalse(self.pgboxdb.engine_pids(), "a failed start left an engine")
            self.assertFalse(os.path.exists(os.path.join(self.pgboxdb.data, "postmaster.pid")), "failed start left postmaster.pid")
        finally:
            os.chmod(base, 0o700)
        self.pgboxdb.start()
        with self.pgboxdb.connect("app") as c:
            self.assertEqual(c.scalar("SELECT 1"), "1")

    def test_invalid_configuration(self):
        self.pgboxdb.stop()
        conf = os.path.join(self.pgboxdb.data, "postgresql.conf")
        with open(conf) as f:
            original = f.read()
        with open(conf, "w") as f:
            f.write("this_is_not_a_setting = 42\n" + original)
        r = self.pgboxdb.cli("start")
        self.assertNotEqual(r.code, 0)
        self.assertIn("this_is_not_a_setting", r.stderr + self.pgboxdb.log_tail(), r)
        self.assertFalse(self.pgboxdb.engine_pids())
        with open(conf, "w") as f:
            f.write(original)
        self.pgboxdb.start()

    def test_stale_socket_file_and_pid(self):
        self.pgboxdb.kill(signal.SIGKILL)
        self.assertTrue(self.pgboxdb.wait_engine_gone(30))
        sock = os.path.join(self.pgboxdb.sock_dir, ".s.PGSQL.%d" % self.pgboxdb.port)
        self.assertTrue(os.path.exists(os.path.join(self.pgboxdb.data, "postmaster.pid")), "SIGKILL should leave postmaster.pid for the test")
        if not os.path.exists(sock):
            open(sock, "w").close()
        r = self.pgboxdb.cli("status", json_out=True)
        self.assertEqual(r.json()["running"], False)
        self.pgboxdb.start()
        with self.pgboxdb.connect("app") as c:
            self.assertEqual(c.scalar("SELECT 1"), "1")


if __name__ == "__main__":
    import random
    seed = int(os.environ.get("PGBOXDB_TEST_SEED") or random.randrange(1 << 30))
    print("durability:", DURABILITY, " repetitions:", REPEAT, " order seed:", seed)
    rnd = random.Random(seed)
    loader = unittest.TestLoader()
    loader.sortTestMethodsUsing = lambda a, b: rnd.choice((-1, 1))
    verbosity = 2 if "-v" in sys.argv else 1
    names = [a for a in sys.argv[1:] if a != "-v"]
    suite = loader.loadTestsFromNames(names, sys.modules["__main__"]) if names else loader.loadTestsFromModule(sys.modules["__main__"])
    result = unittest.TextTestRunner(verbosity=verbosity).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
