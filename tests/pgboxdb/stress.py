#!/usr/bin/env python3
"""Acceptance stress workloads with resource-leak monitoring (plan Task 12;
brief §6H). Bounded, with explicit cleanup.

    PGBOXDB_DIST=<prefix> python3 -I tests/pgboxdb/stress.py \
        [--cycles 1000] [--clients 100] [--duration 60] [--startstop 50] [--soak SECONDS]

Samples the engine's RSS, file-descriptor count and thread count throughout,
and reports growth. A steady climb across thousands of create/drop or
clone/drop cycles is a leak; a flat line is not.
"""
import argparse
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import Pgboxdb  # noqa: E402


def proc_sample(pid):
    rss = fd = thr = None
    if sys.platform == "darwin":
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
        rss = int(out) * 1024 if out else None
        fd = len(subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines())
        thr_out = subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()
        thr = max(len(thr_out) - 1, 0)
    else:
        try:
            with open("/proc/%d/status" % pid) as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        rss = int(line.split()[1]) * 1024
                    elif line.startswith("Threads:"):
                        thr = int(line.split()[1])
            fd = len(os.listdir("/proc/%d/fd" % pid))
        except OSError:
            pass
    return {"rss": rss, "fd": fd, "threads": thr}


class Monitor:
    def __init__(self, pgboxdb, interval=2.0):
        self.pgboxdb = pgboxdb
        self.interval = interval
        self.samples = []
        self._stop = False
        self._th = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while not self._stop:
            pid = self.pgboxdb.engine_pid()
            if pid:
                s = proc_sample(pid)
                s["t"] = time.time()
                self.samples.append(s)
            time.sleep(self.interval)

    def __enter__(self):
        self._th.start()
        return self

    def __exit__(self, *exc):
        self._stop = True
        self._th.join(5)

    def report(self, label):
        def series(k):
            return [s[k] for s in self.samples if s.get(k) is not None]
        out = {"label": label, "samples": len(self.samples)}
        for k in ("rss", "fd", "threads"):
            v = series(k)
            if v:
                out[k] = {"first": v[0], "last": v[-1], "max": max(v), "min": min(v), "growth": v[-1] - v[0]}
        return out


def workload_create_drop(pgboxdb, cycles, mon):
    print("  create/drop x %d ..." % cycles)
    with pgboxdb.connect("postgres") as c:
        for i in range(cycles):
            c.query('CREATE DATABASE "s_cd"')
            c.query('DROP DATABASE "s_cd"')
            if i % 200 == 0 and i:
                print("    %d/%d" % (i, cycles))
    known = set(pgboxdb.sql("postgres", "SELECT oid::text FROM pg_database").rows and [r[0] for r in pgboxdb.sql("postgres", "SELECT oid::text FROM pg_database").rows])
    orphans = [d for d in pgboxdb.base_dirs() if d.isdigit() and d not in known]
    return {"cycles": cycles, "orphans": orphans}


def workload_clone_drop(pgboxdb, cycles, mon):
    print("  clone/drop x %d ..." % cycles)
    pgboxdb.cli("create", "tpl", check=True)
    with pgboxdb.connect("tpl") as c:
        c.query("CREATE TABLE t (id int PRIMARY KEY, v text); INSERT INTO t SELECT g, md5(g::text) FROM generate_series(1, 20000) g")
        digest = c.scalar("SELECT md5(string_agg(id || v, ',' ORDER BY id)) FROM t")
    before_base = _base_size(pgboxdb)
    with pgboxdb.connect("postgres") as c:
        for i in range(cycles):
            c.query('CREATE DATABASE "s_cl" TEMPLATE tpl STRATEGY = FILE_COPY')
            c.query('DROP DATABASE "s_cl"')
            if i % 200 == 0 and i:
                print("    %d/%d" % (i, cycles))
    # one surviving clone must still match
    pgboxdb.cli("clone", "tpl", "verify", check=True)
    with pgboxdb.connect("verify") as c:
        ok = c.scalar("SELECT md5(string_agg(id || v, ',' ORDER BY id)) FROM t") == digest
    pgboxdb.cli("drop", "verify", check=True)
    pgboxdb.cli("drop", "tpl", check=True)
    after_base = _base_size(pgboxdb)
    return {"cycles": cycles, "integrity_ok": ok, "base_growth_mb": round((after_base - before_base) / 1048576, 1)}


def _base_size(pgboxdb):
    base = os.path.join(pgboxdb.data, "base")
    total = 0
    for root, _d, files in os.walk(base):
        for f in files:
            try:
                total += os.lstat(os.path.join(root, f)).st_size
            except OSError:
                pass
    return total


def workload_clients(pgboxdb, clients, duration, mon):
    print("  %d clients for %ds ..." % (clients, duration))
    pgboxdb.cli("create", "load", check=True)
    with pgboxdb.connect("load") as c:
        c.query("CREATE TABLE l (client int, n bigint)")
    stop = time.time() + duration
    errors = []
    counts = [0] * clients

    def worker(k):
        try:
            with pgboxdb.connect("load") as c:
                while time.time() < stop:
                    c.query("INSERT INTO l VALUES (%d, 1)" % k)
                    c.scalar("SELECT count(*) FROM l WHERE client = %d" % k)
                    counts[k] += 1
        except Exception as e:  # noqa: BLE001
            errors.append(repr(e))

    threads = [threading.Thread(target=worker, args=(k,)) for k in range(clients)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(duration + 60)
    total = pgboxdb.sql("load", "SELECT count(*) FROM l").scalar()
    pgboxdb.cli("drop", "load", "--force", check=True)
    return {"clients": clients, "duration": duration, "errors": errors[:5], "ops": sum(counts), "rows": int(total)}


def workload_startstop(pgboxdb, n, mon):
    print("  start/stop x %d ..." % n)
    pgboxdb.cli("create", "persist", check=True)
    with pgboxdb.connect("persist") as c:
        c.query("CREATE TABLE k (v text); INSERT INTO k VALUES ('keep')")
    fails = []
    for i in range(n):
        pgboxdb.stop()
        r = pgboxdb.cli("start")
        if r.code != 0:
            fails.append((i, r.stderr.strip()[:80]))
            pgboxdb.cli("start", check=True)
    ok = pgboxdb.sql("persist", "SELECT v FROM k").scalar() == "keep"
    pgboxdb.cli("drop", "persist", check=True)
    return {"cycles": n, "failures": fails, "data_ok": ok}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cycles", type=int, default=1000)
    ap.add_argument("--clients", type=int, default=100)
    ap.add_argument("--duration", type=int, default=60)
    ap.add_argument("--startstop", type=int, default=50)
    ap.add_argument("--soak", type=int, default=0, help="extra mixed-load seconds (optional)")
    args = ap.parse_args()

    import json
    report = {"args": vars(args), "workloads": {}, "monitor": {}}
    problems = []
    with Pgboxdb("stress").up() as pgboxdb:
        with Monitor(pgboxdb) as mon:
            report["workloads"]["create_drop"] = workload_create_drop(pgboxdb, args.cycles, mon)
            report["monitor"]["after_create_drop"] = mon.report("after_create_drop")
            if report["workloads"]["create_drop"]["orphans"]:
                problems.append("create/drop left orphans: %s" % report["workloads"]["create_drop"]["orphans"])

            report["workloads"]["clone_drop"] = workload_clone_drop(pgboxdb, args.cycles, mon)
            report["monitor"]["after_clone_drop"] = mon.report("after_clone_drop")
            cd = report["workloads"]["clone_drop"]
            if not cd["integrity_ok"]:
                problems.append("clone integrity failed after %d cycles" % args.cycles)
            if cd["base_growth_mb"] > 100:
                problems.append("base/ grew %.1f MB over clone/drop cycles (leak)" % cd["base_growth_mb"])

            report["workloads"]["clients"] = workload_clients(pgboxdb, args.clients, args.duration, mon)
            report["monitor"]["after_clients"] = mon.report("after_clients")
            if report["workloads"]["clients"]["errors"]:
                problems.append("client load errors: %s" % report["workloads"]["clients"]["errors"])

            report["workloads"]["startstop"] = workload_startstop(pgboxdb, args.startstop, mon)
            if report["workloads"]["startstop"]["failures"]:
                problems.append("start/stop failures: %s" % report["workloads"]["startstop"]["failures"])
            if not report["workloads"]["startstop"]["data_ok"]:
                problems.append("data lost across start/stop cycles")

            if args.soak:
                report["workloads"]["soak"] = workload_clients(pgboxdb, max(10, args.clients // 2), args.soak, mon)

            # Leak verdict: fd and thread counts must not climb across the whole run.
            full = mon.report("full_run")
            report["monitor"]["full_run"] = full
            for k, cap in (("fd", 64), ("threads", 32)):
                if k in full and full[k]["growth"] > cap:
                    problems.append("%s grew by %d over the run (first %s last %s): possible leak" % (k, full[k]["growth"], full[k]["first"], full[k]["last"]))

    report["problems"] = problems
    print(json.dumps(report, indent=1))
    if problems:
        print("FAIL:", *problems, sep="\n  ")
        return 1
    print("PASS: stress workloads completed with no leaks or integrity failures")
    return 0


if __name__ == "__main__":
    sys.exit(main())
