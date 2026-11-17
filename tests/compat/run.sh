#!/bin/sh
# Behavioural compatibility suite: every step runs on PGBoxDB and, when a
# PostgreSQL 18 control is available, on PostgreSQL too; rows and SQLSTATEs
# must match. Minitest + the pg gem (ruby, libpq).
#
#   PGBOXDB_DIST=<prefix> [PG18_BINDIR=<dir with initdb,pg_ctl>] tests/compat/run.sh
#
# Without PG18_BINDIR the suite asserts PostgreSQL's known behaviour only and
# prints `COVERAGE GAP: no PostgreSQL control`.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
[ -n "${PGBOXDB_DIST:-}" ] || { echo "PGBOXDB_DIST is not set (dist/assemble.sh --out ...)" >&2; exit 2; }
command -v ruby >/dev/null || { echo "COVERAGE GAP: ruby not installed" >&2; exit 2; }
ruby -e 'require "pg"; require "minitest"' 2>/dev/null || { echo "COVERAGE GAP: ruby gems pg/minitest missing (gem install pg minitest)" >&2; exit 2; }

SCRATCH="${PGBOXDB_TEST_SCRATCH:-/tmp/pgbt}"; mkdir -p "$SCRATCH"
export PGBOXDB_HOME="$SCRATCH/compat-$$"
CONTROL_DIR="$SCRATCH/compat-ctl-$$"
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
PORT="$(free_port)"
cleanup() {
  "$PGBOXDB_DIST/bin/pgboxdb" stop --force >/dev/null 2>&1 || true
  [ -n "${PG18_BINDIR:-}" ] && [ -d "$CONTROL_DIR" ] && "$PG18_BINDIR/pg_ctl" -D "$CONTROL_DIR" -m immediate stop >/dev/null 2>&1 || true
  [ -n "${PGBOXDB_TEST_KEEP:-}" ] || rm -rf "$PGBOXDB_HOME" "$CONTROL_DIR"
}
trap cleanup EXIT INT TERM

"$PGBOXDB_DIST/bin/pgboxdb" init --port "$PORT" >/dev/null
"$PGBOXDB_DIST/bin/pgboxdb" start >/dev/null
"$PGBOXDB_DIST/bin/pgboxdb" create compat >/dev/null
PGBOXDB_COMPAT_URL="$("$PGBOXDB_DIST/bin/pgboxdb" url compat)"; export PGBOXDB_COMPAT_URL

if [ -n "${PG18_BINDIR:-}" ]; then
  CPORT="$(free_port)"
  "$PG18_BINDIR/initdb" -D "$CONTROL_DIR" --no-locale -E UTF8 -U postgres -A trust >/dev/null
  "$PG18_BINDIR/pg_ctl" -D "$CONTROL_DIR" -o "-p $CPORT -k $CONTROL_DIR -c listen_addresses=127.0.0.1" -l "$CONTROL_DIR/log" -w start >/dev/null
  "$PG18_BINDIR/createdb" -h 127.0.0.1 -p "$CPORT" -U postgres pgboxdb_compat_control
  PGBOXDB_COMPAT_CONTROL_URL="postgres://postgres@127.0.0.1:$CPORT/pgboxdb_compat_control"; export PGBOXDB_COMPAT_CONTROL_URL
  echo "control: PostgreSQL $("$PG18_BINDIR/postgres" --version | awk '{print $3}') on port $CPORT"
else
  echo "COVERAGE GAP: no PostgreSQL control (set PG18_BINDIR); asserting known PostgreSQL behaviour only"
fi
echo "target:  $("$PGBOXDB_DIST/bin/pgboxdb" version | head -1)"

cd "$HERE"
set +e
ruby -I. -e 'require "minitest/autorun"; Dir["./*_test.rb"].sort.each { |f| require f }' -- "$@"
status=$?
set -e
exit $status
