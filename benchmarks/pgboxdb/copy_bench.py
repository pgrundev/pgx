#!/usr/bin/env python3
"""Benchmark `pgboxdb copy` and branching from the copied template.

Measures, for each source database given:
  import duration (pgboxdb copy), peak RSS of pgboxdb + pg_dump + pg_restore and of
  the engine during the import, peak temp-dump size under $PGBOXDB_HOME/tmp,
  clone latency for N clones, physical disk used by the clones (df delta),
  disk after the first full read of every table in every clone (hint-bit
  amplification), and disk after a representative write in every clone.
Optionally repeats the clone section from a template copied with
--no-optimize, to show what VACUUM (FREEZE, ANALYZE) buys.

Usage:
  copy_bench.py --pgboxdb <bin/pgboxdb> --source-url postgres://u:pw@host:port/db \
                [--dest NAME] [--clones 10 100] [--compare-no-optimize] \
                [--json out.json]
Environment: PGBOXDB_HOME (the runtime to use; must be running), PGBOXDB_PG_BINDIR.
Standard library only. Numbers are what this machine did; record the host.
"""
import argparse
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time


def sh(args, env=None, check=True, input=None):
    r = subprocess.run(args, capture_output=True, text=True, env=env, input=input)
    if check and r.returncode != 0:
        raise SystemExit(f"command failed ({r.returncode}): {' '.join(args)}\n{r.stdout}\n{r.stderr}")
    return r


def df_used(path):
    st = os.statvfs(path)
    return (st.f_blocks - st.f_bavail) * st.f_frsize


def df_free(path):
    st = os.statvfs(path)
    return st.f_bavail * st.f_frsize


def du(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total


def rss_of(pids):
    if not pids:
        return 0
    r = subprocess.run(["ps", "-o", "rss=", "-p", ",".join(str(p) for p in pids)], capture_output=True, text=True)
    return sum(int(x) * 1024 for x in r.stdout.split())


def pids_matching(pattern):
    r = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True)
    return [int(x) for x in r.stdout.split()]


class Sampler(threading.Thread):
    """Samples RSS of the copy process tree and the engine, and the temp dir size."""

    def __init__(self, tmp_dir, engine_pid):
        super().__init__(daemon=True)
        self.tmp_dir = tmp_dir
        self.engine_pid = engine_pid
        self.peak_tools = 0
        self.peak_engine = 0
        self.peak_tmp = 0
        self.stop = threading.Event()

    def run(self):
        while not self.stop.is_set():
            tools = pids_matching(r"pgboxdb copy|pg_dump|pg_restore")
            self.peak_tools = max(self.peak_tools, rss_of(tools))
            self.peak_engine = max(self.peak_engine, rss_of([self.engine_pid]))
            if os.path.isdir(self.tmp_dir):
                self.peak_tmp = max(self.peak_tmp, du(self.tmp_dir))
            time.sleep(0.2)


def psql_url(pgboxdb, name):
    return sh([pgboxdb, "url", name]).stdout.strip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pgboxdb", required=True)
    ap.add_argument("--source-url", required=True)
    ap.add_argument("--dest", default="bench_tpl")
    ap.add_argument("--clones", type=int, nargs="+", default=[10, 100])
    ap.add_argument("--compare-no-optimize", action="store_true")
    ap.add_argument("--psql", default=os.path.join(os.environ.get("PGBOXDB_PG_BINDIR", ""), "psql"))
    ap.add_argument("--json")
    ap.add_argument("--label", default="b", help="prefix for clone names")
    ap.add_argument("--min-free-gb", type=float, default=2.0,
                    help="abort a clone round when free disk under $PGBOXDB_HOME drops below this")
    a = ap.parse_args()

    home = os.environ.get("PGBOXDB_HOME", os.path.expanduser("~/.pgboxdb"))
    status = json.loads(sh([a.pgboxdb, "--json", "status"]).stdout)
    engine_pid = status["pid"]
    data_dir = os.path.join(home, "data")
    out = {"host": os.uname().nodename, "platform": sys.platform, "pgboxdb_home": home, "results": []}

    def run_copy(dest, extra):
        sh([a.pgboxdb, "drop", dest, "--force"], check=False)
        pre_used = df_used(data_dir)
        s = Sampler(os.path.join(home, "tmp"), engine_pid)
        s.start()
        t0 = time.monotonic()
        r = subprocess.run([a.pgboxdb, "--json", "copy", "--from", a.source_url, "--to", dest] + extra,
                           capture_output=True, text=True)
        elapsed = time.monotonic() - t0
        s.stop.set()
        s.join()
        if r.returncode != 0:
            raise SystemExit(f"pgboxdb copy failed: {r.stdout}\n{r.stderr}")
        j = json.loads(r.stdout)
        return {
            "args": extra,
            "import_s": round(elapsed, 2),
            "reported_duration_ms": j["duration_ms"],
            "stages": {st["name"]: st["ms"] for st in j["stages"]},
            "template_size_bytes": j["destination"]["size_bytes"],
            "disk_delta_bytes": df_used(data_dir) - pre_used,
            "peak_rss_tools_bytes": s.peak_tools,
            "peak_rss_engine_bytes": s.peak_engine,
            "peak_tmp_bytes": s.peak_tmp,
        }

    def guard_free():
        free = df_free(data_dir)
        if free < a.min_free_gb * (1 << 30):
            raise SystemExit(f"aborting: only {free / (1 << 30):.1f} GB free under {data_dir} (--min-free-gb {a.min_free_gb})")

    def tables(db):
        r = sh([a.psql, psql_url(a.pgboxdb, db), "-Atc",
                "SELECT quote_ident(schemaname)||'.'||quote_ident(tablename) FROM pg_tables WHERE schemaname NOT IN ('pg_catalog','information_schema')"])
        return r.stdout.split()

    def clone_round(tpl, n, label):
        names = [f"{label}-{i:04d}" for i in range(n)]
        for nm in names:
            sh([a.pgboxdb, "drop", nm, "--force"], check=False)
        # Let APFS/df settle before measuring.
        time.sleep(2)
        used0 = df_used(data_dir)
        lat = []
        for nm in names:
            guard_free()
            t0 = time.monotonic()
            j = json.loads(sh([a.pgboxdb, "--json", "clone", tpl, nm, "--ttl", "2h"]).stdout)
            lat.append(time.monotonic() - t0)
        method = j["method"]
        time.sleep(2)
        used_after_clone = df_used(data_dir)
        # First full read of every table in every clone.
        tbls = tables(names[0])
        read_sql = "; ".join(f"SELECT count(*) FROM {t}" for t in tbls)
        t0 = time.monotonic()
        for nm in names:
            guard_free()
            sh([a.psql, psql_url(a.pgboxdb, nm), "-Atqc", read_sql])
        read_s = time.monotonic() - t0
        sh([a.psql, psql_url(a.pgboxdb, "postgres"), "-Atqc", "CHECKPOINT"])
        time.sleep(2)
        used_after_read = df_used(data_dir)
        # A representative write: update ~2% of the rows of the largest table.
        big = sh([a.psql, psql_url(a.pgboxdb, names[0]), "-Atc",
                  "SELECT quote_ident(schemaname)||'.'||quote_ident(tablename) FROM pg_tables WHERE schemaname NOT IN ('pg_catalog','information_schema') ORDER BY pg_total_relation_size(quote_ident(schemaname)||'.'||quote_ident(tablename)) DESC LIMIT 1"]).stdout.strip()
        # A text column that no index covers, so the update is HOT-eligible and
        # touches heap pages only; otherwise a DELETE of the same range.
        col = sh([a.psql, psql_url(a.pgboxdb, names[0]), "-Atc",
                  f"SELECT a.attname FROM pg_attribute a WHERE a.attrelid = '{big}'::regclass AND a.attnum > 0 AND NOT a.attisdropped "
                  f"AND a.atttypid IN ('text'::regtype,'varchar'::regtype) "
                  f"AND NOT EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND a.attnum = ANY(i.indkey)) LIMIT 1"]).stdout.strip()
        # A contiguous 2% of the table by physical order (what an agent's
        # fixture load or a migration backfill of recent rows does). A scattered
        # update (`WHERE random() < 0.02`) touches nearly every page and
        # un-shares almost the whole table per clone, which filled the disk in
        # the first run of this script; that worst case is noted in benchmarks.md.
        pages = int(sh([a.psql, psql_url(a.pgboxdb, names[0]), "-Atc",
                        f"SELECT relpages FROM pg_class WHERE oid = '{big}'::regclass"]).stdout.strip() or "0")
        last_page = max(pages // 50, 1)
        where = f"ctid < '({last_page},0)'::tid"
        write_sql = f"UPDATE {big} SET {col} = {col} || 'x' WHERE {where}" if col else f"DELETE FROM {big} WHERE {where}"
        try:
            t0 = time.monotonic()
            for nm in names:
                guard_free()
                sh([a.psql, psql_url(a.pgboxdb, nm), "-Atqc", write_sql])
            write_s = time.monotonic() - t0
            sh([a.psql, psql_url(a.pgboxdb, "postgres"), "-Atqc", "CHECKPOINT"])
            time.sleep(2)
            used_after_write = df_used(data_dir)
        finally:
            for nm in names:
                sh([a.pgboxdb, "drop", nm, "--force"], check=False)
        return {
            "clones": n,
            "method": method,
            "clone_ms_p50": round(statistics.median(lat) * 1000, 1),
            "clone_ms_p95": round(sorted(lat)[int(len(lat) * 0.95) - 1 if n > 1 else 0] * 1000, 1),
            "clone_ms_max": round(max(lat) * 1000, 1),
            "disk_after_clone_bytes": used_after_clone - used0,
            "first_read_total_s": round(read_s, 2),
            "disk_after_first_read_bytes": used_after_read - used0,
            "write_sql": write_sql,
            "write_total_s": round(write_s, 2),
            "disk_after_write_bytes": used_after_write - used0,
        }

    variants = [("optimized", [])]
    if a.compare_no_optimize:
        variants.append(("no-optimize", ["--no-optimize"]))
    for label, extra in variants:
        dest = a.dest if label == "optimized" else a.dest + "_noopt"
        res = {"variant": label, "copy": run_copy(dest, extra), "clone_rounds": []}
        print(json.dumps({"variant": label, "copy": res["copy"]}, indent=1), flush=True)
        for n in a.clones:
            cr = clone_round(dest, n, f"{a.label}{label[:3]}")
            res["clone_rounds"].append(cr)
            print(json.dumps(cr, indent=1), flush=True)
        out["results"].append(res)
    if a.json:
        with open(a.json, "w") as f:
            json.dump(out, f, indent=1)
    print("done")


if __name__ == "__main__":
    main()
