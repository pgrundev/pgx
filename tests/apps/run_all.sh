#!/bin/sh
# Runs each application fixture against the assembled PGBoxDB distribution: create a
# database with pgboxdb, hand the app its DATABASE_URL, run it, drop the database.
#
#   PGBOXDB_DIST=<prefix> tests/apps/run_all.sh [app ...]   (default: all)
#
# Prints one line per app: PASS, FAIL or COVERAGE GAP. Exit 0 only if no app FAILED
# (coverage gaps do not fail the run; they are reported).
set -u
[ -n "${PGBOXDB_DIST:-}" ] || { echo "PGBOXDB_DIST is not set (dist/assemble.sh --out ...)" >&2; exit 2; }
HERE="$(cd "$(dirname "$0")" && pwd)"
PGBOXDB_BIN="$PGBOXDB_DIST/bin/pgboxdb"
APPS="${*:-psql node python go rails}"
SCRATCH="${PGBOXDB_TEST_SCRATCH:-/tmp/pgbt}"; mkdir -p "$SCRATCH"
export PGBOXDB_HOME="$SCRATCH/apps-$$"
export PGBOXDB_RUN_DIR="/tmp/pgba$$"
cleanup() { "$PGBOXDB_BIN" stop --force >/dev/null 2>&1 || true; [ -n "${PGBOXDB_TEST_KEEP:-}" ] || rm -rf "$PGBOXDB_HOME" "$PGBOXDB_RUN_DIR"; }
trap cleanup EXIT INT TERM
"$PGBOXDB_BIN" init >/dev/null
"$PGBOXDB_BIN" start >/dev/null
echo "runtime: $("$PGBOXDB_BIN" version | head -1)"
fail=0
for app in $APPS; do
  [ -x "$HERE/$app/run.sh" ] || { echo "APP $app: COVERAGE GAP (no fixture)"; continue; }
  "$PGBOXDB_BIN" drop "${app}_fx" --force >/dev/null 2>&1 || true
  "$PGBOXDB_BIN" create "${app}_fx" >/dev/null
  URL="$("$PGBOXDB_BIN" url "${app}_fx")"
  DATABASE_URL="$URL" PGBOXDB_TEST_PSQL="${PGBOXDB_TEST_PSQL:-}" sh "$HERE/$app/run.sh"
  rc=$?
  [ $rc -eq 1 ] && fail=1
  "$PGBOXDB_BIN" drop "${app}_fx" --force >/dev/null 2>&1 || true
done
exit $fail
