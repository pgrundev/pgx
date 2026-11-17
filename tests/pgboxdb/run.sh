#!/bin/sh
# Runs the Python black-box suites against an assembled distribution.
#   PGBOXDB_DIST=<prefix> tests/pgboxdb/run.sh [suite ...]   (default: all quick suites)
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
[ -n "${PGBOXDB_DIST:-}" ] || { echo "PGBOXDB_DIST is not set (dist/assemble.sh --out ...)" >&2; exit 2; }
SUITES="${*:-test_sql.py clone_consistency.py test_isolation.py test_crash.py test_storage.py test_concurrency.py}"
OUT="${PGBOXDB_TEST_RESULTS:-$HERE/../results/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$OUT"
status=0
for s in $SUITES; do
  [ -f "$HERE/$s" ] || { echo "no such suite: $s" >&2; status=1; continue; }
  echo "=== $s"
  if python3 -I "$HERE/$s" "$@" >"$OUT/${s%.py}.log" 2>&1; then echo "PASS $s"; else echo "FAIL $s (see $OUT/${s%.py}.log)"; status=1; fi
  tail -3 "$OUT/${s%.py}.log"
done
echo "results in $OUT"
exit $status
