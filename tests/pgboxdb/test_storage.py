#!/usr/bin/env python3
"""Storage suite for the portable backend (plan Task 11; brief §6G).

    PGBOXDB_DIST=<prefix> python3 -I tests/pgboxdb/test_storage.py [-v]

Runs on whatever filesystem holds PGBOXDB_TEST_SCRATCH (APFS on macOS; ext4 or
tmpfs on Linux CI). The low-disk cases use a small disk image on macOS
(hdiutil, no root) and print `COVERAGE GAP: low-disk (no small volume)`
elsewhere. ZFS is covered by storage_zfs.sh on a host with a ZFS dataset.
"""
import hashlib
import os
import shutil
import subprocess
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import PGBoxDB, scratch_root  # noqa: E402

CYCLES = int(os.environ.get("PGBOXDB_TEST_CYCLES", "200"))


def free_bytes(path):
    st = os.statvfs(path)
    return st.f_bavail * st.f_frsize


def dir_tree_size(path):
    total = 0
    for root, _dirs, files in os.walk(path):
        for f in files:
            try:
                total += os.lstat(os.path.join(root, f)).st_size
            except OSError:
                pass
    return total


def digest(conn, table):
    return conn.scalar("SELECT md5(string_agg(id || ':' || v, ',' ORDER BY id)) FROM %s" % table)


class SmallVolume:
    """A ~192 MB APFS disk image mounted without root (macOS only)."""

    def __init__(self, size_mb=192):
        self.mount = None
        self.image = os.path.join(scratch_root(), "small-%d.dmg" % os.getpid())
        if sys.platform != "darwin":
            return
        subprocess.run(["hdiutil", "create", "-quiet", "-size", "%dm" % size_mb, "-fs", "APFS", "-volname", "pgbsmall%d" % os.getpid(), self.image], check=True, capture_output=True)
        out = subprocess.run(["hdiutil", "attach", "-nobrowse", self.image], check=True, capture_output=True, text=True).stdout
        for line in out.splitlines():
            if "/Volumes/" in line:
                self.mount = line.split("\t")[-1].strip()
        if not self.mount:
            raise RuntimeError("could not find mount point in %r" % out)

    def close(self):
        if self.mount:
            for _ in range(10):
                if subprocess.run(["hdiutil", "detach", "-quiet", "-force", self.mount], capture_output=True).returncode == 0:
                    break
                time.sleep(0.5)
        if os.path.exists(self.image):
            os.remove(self.image)


class StorageCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pgboxdb = PGBoxDB("stor").up()
        cls.pgboxdb.cli("create", "src", check=True)
        with cls.pgboxdb.connect("src") as c:
            c.query("CREATE TABLE t (id int PRIMARY KEY, v text)")
            c.query("INSERT INTO t SELECT g, md5(g::text) FROM generate_series(1, 300000) g")
            c.query("CHECKPOINT")
            cls.src_digest = digest(c, "t")
            cls.src_size = int(c.scalar("SELECT pg_database_size('src')"))
        cls.block_clone = cls.pgboxdb.cli("doctor", json_out=True).json()
        cls.block_clone = any(c["name"] == "filesystem" and "block cloning:" in c["detail"] and c["status"] == "ok" for c in cls.block_clone["checks"])

    @classmethod
    def tearDownClass(cls):
        cls.pgboxdb.cleanup()

    def test_clone_drop_cycles_keep_integrity_and_leak_nothing(self):
        before_dirs = self.pgboxdb.base_dirs()
        # Size of PGBoxDB's own base/ tree, not the shared volume's free space, so
        # unrelated activity on the machine cannot be mistaken for a leak.
        base = os.path.join(self.pgboxdb.data, "base")
        before_base = dir_tree_size(base)
        t0 = time.time()
        for i in range(CYCLES):
            name = "cy%d" % i
            r = self.pgboxdb.cli("clone", "src", name, json_out=True)
            self.assertEqual(r.code, 0, "cycle %d: %r" % (i, r))
            if i % 25 == 0:
                with self.pgboxdb.connect(name) as c:
                    self.assertEqual(digest(c, "t"), self.src_digest, "cycle %d: clone differs from source" % i)
                    c.query("INSERT INTO t VALUES (-1, 'clone write')")
            r = self.pgboxdb.cli("drop", name)
            self.assertEqual(r.code, 0, "cycle %d drop: %r" % (i, r))
        elapsed = time.time() - t0
        self.assertEqual(self.pgboxdb.base_dirs(), before_dirs, "clone/drop cycles leaked base directories")
        with self.pgboxdb.connect("src") as c:
            self.assertEqual(digest(c, "t"), self.src_digest, "source changed during cycles")
        after_base = dir_tree_size(base)
        print("\n  %d clone/drop cycles in %.1fs (%.0f ms each); base/ grew %+.1f MB" % (CYCLES, elapsed, elapsed * 1000 / CYCLES, (after_base - before_base) / 1048576))
        # Every clone was dropped: base/ must return to its starting size, within
        # one source's worth of slack for a clone whose drop the janitor has queued.
        self.assertLess(after_base - before_base, self.src_size * 2, "base/ grew by more than two source copies: dropped clones are not reclaimed")

    def test_clone_disk_cost_matches_backend(self):
        # A large source so the copy-vs-clone difference (~hundreds of MB) dwarfs
        # the filesystem noise from WAL recycling, autovacuum and checkpoints
        # (tens of MB). A single small clone cannot be measured reliably on a
        # live filesystem.
        self.pgboxdb.cli("create", "big", check=True)
        with self.pgboxdb.connect("big") as c:
            c.query("CREATE TABLE b (id int PRIMARY KEY, p text)")
            c.query("INSERT INTO b SELECT g, repeat('x', 220) FROM generate_series(1, 1000000) g")
            c.query("CHECKPOINT")
            big_size = int(c.scalar("SELECT pg_database_size('big')"))
        time.sleep(1.0)
        before = free_bytes(self.pgboxdb.data)
        self.pgboxdb.cli("clone", "big", "bigcost", check=True)
        with self.pgboxdb.connect("bigcost") as c:
            self.assertEqual(int(c.scalar("SELECT pg_database_size('bigcost')")), big_size, "pg_database_size of a clone must equal the source")
        time.sleep(1.5)  # let checkpoint/WAL settle before the second reading
        delta = before - free_bytes(self.pgboxdb.data)
        print("\n  clone of a %.0f MB database consumed %.0f MB net (%s)" % (big_size / 1048576, delta / 1048576, "block cloning" if self.block_clone else "file copy"))
        if self.block_clone:
            self.assertLess(delta, big_size * 0.5, "block cloning should share most of a %.0f MB source" % (big_size / 1048576))
        else:
            self.assertGreater(delta, big_size * 0.5, "file copy should consume about the source size")
        oid = self.pgboxdb.sql("postgres", "SELECT oid FROM pg_database WHERE datname = 'bigcost'").scalar()
        self.pgboxdb.cli("drop", "bigcost", check=True)
        self.assertFalse(os.path.exists(os.path.join(self.pgboxdb.data, "base", oid)), "drop left the clone's directory")
        self.pgboxdb.cli("drop", "big", check=True)

    def test_clone_then_write_diverges_without_touching_source(self):
        self.pgboxdb.cli("clone", "src", "div", check=True)
        with self.pgboxdb.connect("div") as c:
            c.query("UPDATE t SET v = 'changed' WHERE id % 1000 = 0")
            c.query("DELETE FROM t WHERE id > 290000")
            c.query("VACUUM FULL t")
            self.assertEqual(c.scalar("SELECT count(*) FROM t"), "290000")
        with self.pgboxdb.connect("src") as c:
            self.assertEqual(digest(c, "t"), self.src_digest, "writes to the clone reached the source")
        self.pgboxdb.cli("drop", "div", check=True)

    def test_data_directory_permissions(self):
        self.assertEqual(os.stat(self.pgboxdb.data).st_mode & 0o777, 0o700)
        self.assertEqual(os.stat(os.path.join(self.pgboxdb.home, "config.json")).st_mode & 0o777, 0o600)
        for root, dirs, files in os.walk(os.path.join(self.pgboxdb.data, "base")):
            for d in dirs:
                mode = os.stat(os.path.join(root, d)).st_mode & 0o777
                self.assertEqual(mode & 0o077, 0, "%s is group/world accessible (%o)" % (os.path.join(root, d), mode))
            break

    def test_restart_with_many_databases(self):
        self.pgboxdb.cli("create", "tpl", check=True)
        with self.pgboxdb.connect("tpl") as c:
            c.query("CREATE TABLE k (id int PRIMARY KEY, v text); INSERT INTO k VALUES (1, 'one')")
        n = 150
        t0 = time.time()
        for i in range(n):
            self.pgboxdb.cli("clone", "tpl", "many%d" % i, check=True)
        create_s = time.time() - t0
        t0 = time.time()
        self.pgboxdb.stop()
        self.pgboxdb.start()
        restart_s = time.time() - t0
        for i in range(0, n, 15):
            with self.pgboxdb.connect("many%d" % i) as c:
                self.assertEqual(c.scalar("SELECT v FROM k WHERE id = 1"), "one")
        status = self.pgboxdb.cli("status", json_out=True).json()
        print("\n  %d clones in %.1fs (%.0f ms each); stop+start with %d databases: %.2fs; RSS %.0f MB" % (n, create_s, create_s * 1000 / n, status["databases"], restart_s, (status.get("memory_rss_bytes") or 0) / 1048576))
        self.assertGreaterEqual(status["databases"], n + 2)
        for i in range(n):
            self.pgboxdb.cli("drop", "many%d" % i, check=True)
        self.pgboxdb.cli("drop", "tpl", check=True)


class LowDiskCase(unittest.TestCase):
    """PGBoxDB on a nearly full volume: the engine must report 53100, stay up, and recover."""

    def setUp(self):
        if sys.platform != "darwin":
            self.skipTest("COVERAGE GAP: low-disk (no small volume without root on this platform)")
        self.vol = SmallVolume()
        self.pgboxdb = PGBoxDB("low")
        self.pgboxdb.home = os.path.join(self.vol.mount, "h")
        self.pgboxdb.env["PGBOXDB_HOME"] = self.pgboxdb.home
        os.makedirs(self.pgboxdb.home)
        self.pgboxdb.up()

    def tearDown(self):
        self.pgboxdb.cleanup()
        self.vol.close()

    def test_storage_exhaustion_panics_and_recovers(self):
        """A full disk raises 53100 to the client. The WAL-write failure PANICs,
        and because a PGBoxDB runtime is one crash domain (docs/pgboxdb/v0.1-readiness.md)
        the runtime goes down rather than auto-restarting. This is PostgreSQL's
        WAL-exhaustion behaviour minus the postmaster's auto-restart. After space
        is freed, `pgboxdb start` recovers by WAL replay and committed data persists.
        No corruption, no partial rows."""
        # Ballast we can delete to make room for recovery once the disk is full.
        ballast = os.path.join(self.vol.mount, "ballast.bin")
        with open(ballast, "wb") as f:
            f.write(b"\0" * (24 * 1024 * 1024))
        self.pgboxdb.cli("create", "fill", check=True)
        committed_before = None
        got = None
        try:
            with self.pgboxdb.connect("fill") as c:
                c.query("SET synchronous_commit = on")
                c.query("CREATE TABLE f (id serial PRIMARY KEY, p text)")
                c.query("INSERT INTO f (p) VALUES ('durable-marker')")
                committed_before = c.scalar("SELECT count(*) FROM f")
                for _ in range(400):
                    c.query("INSERT INTO f (p) SELECT repeat('x', 1000) FROM generate_series(1, 2000)")
        except pgclient.DbError as e:
            got = e.sqlstate
        except pgclient.ConnectionClosed:
            got = "closed"
        self.assertEqual(got, "53100", "a full disk must raise disk_full (53100), got %r" % got)
        self.assertEqual(committed_before, "1")
        # One crash domain: the WAL PANIC takes the runtime down. Documented limitation.
        self.pgboxdb.wait_engine_gone(30)
        self.assertFalse(self.pgboxdb.engine_pids(), "runtime still up (would mean the PANIC was contained)")
        log = self.pgboxdb.log_tail(40)
        self.assertIn("No space left on device", log)
        self.assertTrue("PANIC" in log or "terminated by signal" in log, "expected a WAL-write PANIC in the log")
        # Free space and recover.
        os.remove(ballast)
        r = self.pgboxdb.cli("start")
        self.assertEqual(r.code, 0, "recovery after disk-full failed: %r\n%s" % (r, self.pgboxdb.log_tail(20)))
        with self.pgboxdb.connect("fill") as c:
            # The row committed before the crash survived WAL replay.
            self.assertEqual(c.scalar("SELECT p FROM f WHERE id = 1"), "durable-marker")
            # No partial/garbage rows: every row is either the marker or a full 'x'*1000.
            self.assertEqual(c.scalar("SELECT count(*) FROM f WHERE p <> 'durable-marker' AND p <> repeat('x', 1000)"), "0")
            # Writes resume now that there is space.
            c.query("INSERT INTO f (p) VALUES ('after-recovery')")
            self.assertEqual(c.scalar("SELECT count(*) FROM f WHERE p = 'after-recovery'"), "1")
        # A full clean restart still works and keeps the data.
        self.pgboxdb.stop()
        self.pgboxdb.start()
        with self.pgboxdb.connect("fill") as c:
            self.assertEqual(c.scalar("SELECT p FROM f WHERE id = 1"), "durable-marker")


if __name__ == "__main__":
    unittest.main(verbosity=2 if "-v" in sys.argv else 1, argv=[a for a in sys.argv if a != "-v"])
