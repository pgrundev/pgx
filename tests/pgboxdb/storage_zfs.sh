#!/bin/sh
# ZFS storage checks for PGBoxDB: block cloning through `pgboxdb clone`, and snapshot
# consistency (a ZFS snapshot of the data dataset taken mid-workload, cloned,
# and opened by a second runtime). Runs ONLY on a host with a real ZFS dataset
# the current user may snapshot/clone; it never mocks ZFS.
#
#   PGBOXDB_DIST=<prefix> tests/pgboxdb/storage_zfs.sh <pool/dataset>
#
# Exit 0 pass, 1 fail, 2 COVERAGE GAP (no dataset given or zfs unavailable).
set -eu
DATASET="${1:-}"
[ -n "${PGBOXDB_DIST:-}" ] || { echo "PGBOXDB_DIST is not set" >&2; exit 2; }
if [ -z "$DATASET" ] || ! command -v zfs >/dev/null 2>&1; then
  echo "COVERAGE GAP: ZFS — no dataset given or zfs(8) not installed; ZFS behaviour is UNVERIFIED on this host"; exit 2
fi
MNT="$(zfs get -H -o value mountpoint "$DATASET")"
[ -d "$MNT" ] || { echo "dataset $DATASET is not mounted" >&2; exit 2; }
PGBOXDB_BIN="$PGBOXDB_DIST/bin/pgboxdb"
export PGBOXDB_HOME="$MNT/pgboxdb-zfs-$$"
export PGBOXDB_RUN_DIR="/tmp/pgbz$$"
SNAP="$DATASET@pgboxdb-test-$$"
CLONE="$DATASET-pgboxdb-clone-$$"
cleanup() {
  "$PGBOXDB_BIN" stop --force >/dev/null 2>&1 || true
  [ -n "${PGBOXDB2_HOME:-}" ] && PGBOXDB_HOME="$PGBOXDB2_HOME" PGBOXDB_RUN_DIR="$PGBOXDB_RUN_DIR" "$PGBOXDB_BIN" stop --force >/dev/null 2>&1 || true
  zfs destroy -f "$CLONE" >/dev/null 2>&1 || true
  zfs destroy "$SNAP" >/dev/null 2>&1 || true
  rm -rf "$PGBOXDB_HOME" "$PGBOXDB_RUN_DIR"
}
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
psqlq() { python3 - "$@" <<'PY'
import sys, os
sys.path.insert(0, os.path.join(os.environ["REPO"], "tests", "pgboxdb"))
from lib import pgclient
dir_, port, db, sql = sys.argv[1:5]
with pgclient.connect_unix(dir_, int(port), db) as c:
    r = c.query(sql)
    print("\n".join("|".join("" if v is None else v for v in row) for row in r.rows))
PY
}
export REPO="$(cd "$(dirname "$0")/../.." && pwd)"

echo "== init on $DATASET ($MNT), filesystem $(zfs get -H -o value type "$DATASET")"
"$PGBOXDB_BIN" init >/dev/null
"$PGBOXDB_BIN" start >/dev/null
PORT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["port"])' "$PGBOXDB_HOME/config.json")"
"$PGBOXDB_BIN" doctor --json | python3 -c 'import json,sys; d=json.load(sys.stdin); fs=[c for c in d["checks"] if c["name"]=="filesystem"][0]; print("filesystem check:", fs["status"], fs["detail"]); sys.exit(0 if "block cloning" in fs["detail"] and fs["status"]=="ok" else 1)' || fail "pgboxdb doctor does not see block cloning on ZFS (needs the block_cloning feature enabled on the pool)"

echo "== block cloning through pgboxdb clone"
"$PGBOXDB_BIN" create src >/dev/null
psqlq "$PGBOXDB_RUN_DIR" "$PORT" src "CREATE TABLE t AS SELECT g id, md5(g::text) v FROM generate_series(1, 2000000) g; CHECKPOINT" >/dev/null
SRC_MD5="$(psqlq "$PGBOXDB_RUN_DIR" "$PORT" src "SELECT md5(string_agg(id || v, ',' ORDER BY id)) FROM t")"
USED_BEFORE="$(zfs get -Hp -o value used "$DATASET")"
"$PGBOXDB_BIN" clone src c1 >/dev/null
sync; sleep 2
USED_AFTER="$(zfs get -Hp -o value used "$DATASET")"
SRC_BYTES="$(psqlq "$PGBOXDB_RUN_DIR" "$PORT" postgres "SELECT pg_database_size('src')")"
echo "   source $((SRC_BYTES / 1048576)) MB; dataset grew $(( (USED_AFTER - USED_BEFORE) / 1048576 )) MB on clone"
[ $((USED_AFTER - USED_BEFORE)) -lt $((SRC_BYTES / 4)) ] || fail "clone consumed more than a quarter of the source: block cloning not effective"
[ "$(psqlq "$PGBOXDB_RUN_DIR" "$PORT" c1 "SELECT md5(string_agg(id || v, ',' ORDER BY id)) FROM t")" = "$SRC_MD5" ] || fail "clone content differs from source"

echo "== ZFS snapshot consistency under load"
python3 - "$PGBOXDB_RUN_DIR" "$PORT" <<'PY' &
import sys, os, time
sys.path.insert(0, os.path.join(os.environ["REPO"], "tests", "pgboxdb"))
from lib import pgclient
with pgclient.connect_unix(sys.argv[1], int(sys.argv[2]), "src") as c:
    c.query("CREATE TABLE w (id int PRIMARY KEY, b int)")
    b = 0
    end = time.time() + 20
    while time.time() < end:
        b += 1
        c.query("BEGIN"); c.query("INSERT INTO w SELECT g, %d FROM generate_series(%d, %d) g" % (b, (b-1)*500+1, b*500)); c.query("COMMIT")
PY
WRITER=$!
sleep 5
zfs snapshot "$SNAP"
zfs clone "$CLONE" >/dev/null 2>&1 || zfs clone "$SNAP" "$CLONE"
wait $WRITER || true
CMNT="$(zfs get -H -o value mountpoint "$CLONE")"
export PGBOXDB2_HOME="$CMNT/$(basename "$PGBOXDB_HOME")"
rm -f "$PGBOXDB2_HOME/data/postmaster.pid"
# second runtime on the snapshot clone: new port, same run dir
python3 - "$PGBOXDB2_HOME/config.json" <<'PY'
import json, sys, socket
p = sys.argv[1]; d = json.load(open(p)); s = socket.socket(); s.bind(("127.0.0.1", 0)); d["port"] = s.getsockname()[1]; s.close()
json.dump(d, open(p, "w"))
PY
PORT2="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["port"])' "$PGBOXDB2_HOME/config.json")"
PGBOXDB_HOME="$PGBOXDB2_HOME" "$PGBOXDB_BIN" start >/dev/null || fail "runtime on the ZFS snapshot clone did not start (recovery failed)"
ROWS="$(psqlq "$PGBOXDB_RUN_DIR" "$PORT2" src "SELECT count(*), count(DISTINCT b), coalesce(max(b),0) FROM w")"
echo "   snapshot clone: rows|batches|maxbatch = $ROWS"
PARTIAL="$(psqlq "$PGBOXDB_RUN_DIR" "$PORT2" src "SELECT count(*) FROM (SELECT b, count(*) n FROM w GROUP BY b HAVING count(*) <> 500) x")"
[ "$PARTIAL" = "0" ] || fail "$PARTIAL partial batch(es) visible in the snapshot clone: snapshot is not transactionally consistent"
[ "$(psqlq "$PGBOXDB_RUN_DIR" "$PORT2" src "SELECT md5(string_agg(id || v, ',' ORDER BY id)) FROM t")" = "$SRC_MD5" ] || fail "table t differs in the snapshot clone"
echo "PASS: ZFS block cloning and snapshot consistency verified on $DATASET"
