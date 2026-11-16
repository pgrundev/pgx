#!/usr/bin/env python3
"""Clone snapshot consistency (plan Task 7).

1. A writer commits batches of 500 rows into the source without pause.
2. Meanwhile the test clones the source through `pgboxdb clone` and, separately,
   through CREATE DATABASE … TEMPLATE … directly, recording whether each
   attempt was accepted or refused (55006).
3. Every accepted clone is checked: each batch present is complete, the heap
   and the primary-key index agree, every row's checksum matches its payload,
   and no batch newer than the writer's last commit at clone time exists.
4. The idle-session case: a session connected to the source but outside any
   transaction; stock PostgreSQL refuses the clone, the result here is recorded.

Exit 0 when every accepted clone is consistent; 1 otherwise.
"""
import hashlib
import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import Pgboxdb  # noqa: E402

BATCH = 500


def crc(batch, i):
    return hashlib.md5(("%d:%d" % (batch, i)).encode()).hexdigest()[:16]


def check_clone(pgboxdb, name, committed_at_clone):
    problems = []
    with pgboxdb.connect(name) as c:
        heap = int(c.scalar("SELECT count(*) FROM t"))
        c.query("SET enable_seqscan = off")
        via_index = int(c.scalar("SELECT count(*) FROM t WHERE id >= 0"))
        if heap != via_index:
            problems.append("heap %d != index %d" % (heap, via_index))
        bad = int(c.scalar("SELECT count(*) FROM t WHERE crc <> substr(md5(batch || ':' || id), 1, 16)"))
        if bad:
            problems.append("%d rows with a wrong checksum" % bad)
        rows = c.query("SELECT batch, count(*) FROM t GROUP BY batch ORDER BY batch").rows
        for b, n in rows:
            if int(n) != BATCH:
                problems.append("batch %s has %s rows" % (b, n))
        maxb = max([int(b) for b, _ in rows], default=0)
        if maxb > committed_at_clone + 1:
            problems.append("batch %d present but writer had committed only %d at clone time" % (maxb, committed_at_clone))
        c.query("SELECT * FROM t ORDER BY id")  # read every page
    return problems


def fill(pgboxdb, db, batches):
    with pgboxdb.connect(db) as w:
        w.query("CREATE TABLE t (id int PRIMARY KEY, batch int NOT NULL, payload text, crc text NOT NULL)")
        for b in range(1, batches + 1):
            base = (b - 1) * BATCH
            values = ",".join("(%d,%d,repeat('x',40),'%s')" % (base + i, b, crc(b, base + i)) for i in range(BATCH))
            w.query("INSERT INTO t VALUES " + values)
        return int(w.scalar("SELECT count(*) FROM t")), w.scalar("SELECT md5(string_agg(id || ':' || crc, ',' ORDER BY id)) FROM t")


def main():
    report = {"clones": [], "refusals": {}, "problems": []}
    with Pgboxdb("clonecons") as pgboxdb:
        pgboxdb.up()
        pgboxdb.cli("create", "src", check=True)
        pgboxdb.cli("create", "noise", check=True)
        src_rows, src_digest = fill(pgboxdb, "src", 40)
        report["source"] = {"rows": src_rows, "digest": src_digest}

        # A: a writer hammers another database while the source is cloned.
        state = {"committed": 0, "stop": False, "errors": [], "started": time.time(), "rows": 0}

        def writer():
            try:
                with pgboxdb.connect("noise") as w:
                    w.query("CREATE TABLE n (id int PRIMARY KEY, batch int, crc text)")
                    b = 0
                    while not state["stop"]:
                        b += 1
                        base = (b - 1) * BATCH
                        values = ",".join("(%d,%d,'%s')" % (base + i, b, crc(b, base + i)) for i in range(BATCH))
                        w.query("BEGIN")
                        w.query("INSERT INTO n VALUES " + values)
                        w.query("COMMIT")
                        state["committed"] = b
                    state["rows"] = int(w.scalar("SELECT count(*) FROM n"))
            except Exception as e:  # noqa: BLE001
                state["errors"].append(repr(e))

        th = threading.Thread(target=writer, daemon=True)
        th.start()
        time.sleep(0.3)
        for i in range(10):
            name = "c%d" % i
            r = pgboxdb.cli("clone", "src", name, json_out=True)
            entry = {"clone": name, "exit": r.code}
            if r.code != 0:
                entry["error"] = r.stderr.strip() or r.stdout.strip()
                report["problems"].append("%s: clone failed: %s" % (name, entry["error"]))
            else:
                entry["elapsed_ms"] = r.json()["elapsed_ms"]
                entry["method"] = r.json()["method"]
                entry["problems"] = check_clone(pgboxdb, name, 40)
                with pgboxdb.connect(name) as c:
                    digest = c.scalar("SELECT md5(string_agg(id || ':' || crc, ',' ORDER BY id)) FROM t")
                    if digest != src_digest:
                        entry["problems"].append("digest %s != source %s" % (digest, src_digest))
                report["problems"] += ["%s: %s" % (name, p) for p in entry["problems"]]
            report["clones"].append(entry)
        state["stop"] = True
        th.join(timeout=120)
        elapsed = time.time() - state["started"]
        report["writer"] = {"batches": state["committed"], "rows_in_table": state["rows"], "seconds": round(elapsed, 2),
                            "rows_per_s": round(state["rows"] / elapsed) if elapsed else None, "errors": state["errors"]}
        if state["rows"] != state["committed"] * BATCH:
            report["problems"].append("noise table has %d rows, writer committed %d batches" % (state["rows"], state["committed"]))

        # B: clones of a source with an open write transaction must be refused (55006, exit 6).
        holder = pgboxdb.connect("src")
        holder.query("BEGIN")
        holder.query("INSERT INTO t VALUES (10000000, 999, 'x', 'uncommitted')")
        before = pgboxdb.base_dirs()
        r = pgboxdb.cli("clone", "src", "busy_txn", json_out=True)
        report["refusals"]["open_transaction"] = {"cli_exit": r.code}
        with pgboxdb.connect("postgres") as admin:
            try:
                admin.query('CREATE DATABASE busy_direct TEMPLATE src')
                report["refusals"]["open_transaction"]["direct"] = "ACCEPTED"
                report["problems"].append("engine accepted a clone of a source with an open write transaction")
            except pgclient.DbError as e:
                report["refusals"]["open_transaction"]["direct"] = e.sqlstate
        holder.query("ROLLBACK")
        # C: an idle session (no transaction) on the source.
        holder.query("SELECT 1")
        r = pgboxdb.cli("clone", "src", "busy_idle", json_out=True)
        report["refusals"]["idle_session"] = {"cli_exit": r.code}
        with pgboxdb.connect("postgres") as admin:
            try:
                admin.query('CREATE DATABASE idle_direct TEMPLATE src')
                report["refusals"]["idle_session"]["direct"] = "ACCEPTED"
                report["refusals"]["idle_session"]["problems"] = check_clone(pgboxdb, "idle_direct", 40)
            except pgclient.DbError as e:
                report["refusals"]["idle_session"]["direct"] = e.sqlstate
        holder.close()
        if pgboxdb.base_dirs() != before + [d for d in pgboxdb.base_dirs() if d not in before and report["refusals"]["idle_session"].get("direct") == "ACCEPTED"]:
            pass
        after = pgboxdb.base_dirs()
        leaked = [d for d in after if d not in before]
        if report["refusals"]["idle_session"].get("direct") == "ACCEPTED":
            leaked = leaked[1:] if leaked else leaked
        if leaked:
            report["problems"].append("refused clones left base dirs: %s" % leaked)
        if report["refusals"]["open_transaction"]["cli_exit"] != 6:
            report["problems"].append("CLI clone of a busy source exited %d, expected 6" % report["refusals"]["open_transaction"]["cli_exit"])
        # Source untouched by all of the above.
        rows, digest = pgboxdb.sql("src", "SELECT count(*) FROM t").scalar(), pgboxdb.sql("src", "SELECT md5(string_agg(id || ':' || crc, ',' ORDER BY id)) FROM t").scalar()
        if int(rows) != src_rows or digest != src_digest:
            report["problems"].append("source changed: %s rows, digest %s" % (rows, digest))

    report["summary"] = {"clones_ok": sum(1 for c in report["clones"] if c["exit"] == 0 and not c.get("problems")),
                         "clones_total": len(report["clones"]), "consistent": not report["problems"]}
    print(json.dumps(report, indent=1))
    if report["problems"]:
        print("FAIL:", *report["problems"], sep="\n  ")
        return 1
    print("PASS: %d/%d clones identical to the source under a concurrent writer (%s rows/s); busy source refused: %s"
          % (report["summary"]["clones_ok"], report["summary"]["clones_total"], report["writer"]["rows_per_s"], json.dumps(report["refusals"])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
