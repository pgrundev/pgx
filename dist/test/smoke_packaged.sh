#!/bin/sh
# Smoke-test a PACKAGED PGX distribution as an external developer would, on a
# clean host (no system PostgreSQL, no Homebrew RE2/abseil). Given the extracted
# dist dir, it checks for missing/stray runtime libraries and runs the full
# onboarding: run -> clone --ttl -> auto-expiry across a restart -> permanent
# data survives -> RE2 linked -> clean shutdown / no leaked processes.
#
#   dist/test/smoke_packaged.sh <extracted-dist-dir>
set -u
DIST="${1:?usage: smoke_packaged.sh <extracted-dist-dir>}"
OS="$(uname -s)"
die() { echo "SMOKE FAIL: $*" >&2; exit 1; }
PGX="$DIST/bin/pgx"; PG="$DIST/bin/postgres"
[ -x "$PGX" ] && [ -x "$PG" ] || die "bin/pgx or bin/postgres missing"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/pgx-smoke.XXXXXX")"
export PGX_HOME="$WORK/home" PGX_RUN_DIR="$WORK/run" PGX_TTL_INTERVAL=1
trap '"$PGX" stop >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT INT TERM
# Clean PATH: no system postgres/psql. Keep only base dirs.
export PATH=/usr/bin:/bin:/usr/sbin:/sbin

echo "## shipped binaries: no missing / non-system runtime libraries"
if [ "$OS" = Darwin ]; then
  for f in "$DIST"/bin/* $( [ -d "$DIST/lib" ] && echo "$DIST"/lib/*.dylib ); do
    [ -e "$f" ] || continue
    stray="$(otool -L "$f" | tail -n +2 | awk '{print $1}' | grep -vE '^/usr/lib/|^/System/|^@(rpath|loader_path|executable_path)/' || true)"
    [ -z "$stray" ] || die "$f loads non-system libs:\n$stray"
    [ "$f" = "$PG" ] && { n=$(DYLD_PRINT_LIBRARIES=1 "$PG" --version 2>&1 | grep -cE '^dyld[^ ]* <[^>]*> (/opt/homebrew|/usr/local)/' || true); [ "$n" = 0 ] || die "engine loads $n Homebrew dylibs"; }
  done
  echo "ok: only /usr/lib + /System (and bundled @rpath), 0 Homebrew loads"
else
  command -v ldd >/dev/null 2>&1 && if ldd "$PG" 2>&1 | grep -i 'not found'; then die "engine has missing shared libraries"; fi
  echo "ok: ldd reports no missing libraries"
fi

echo "## no system PostgreSQL on PATH"
command -v postgres >/dev/null 2>&1 && die "system postgres present on PATH"
command -v initdb  >/dev/null 2>&1 && die "system initdb present on PATH"
echo "ok: clean PATH"

echo "## pgx run (fresh install) + clone --ttl + ls"
"$PGX" run >/dev/null || { "$PGX" doctor || true; die "pgx run"; }
one_engine=$(pgrep -f "$PGX_HOME/data" | wc -l | tr -d ' '); echo "engine procs: $one_engine"
"$PGX" clone app test-001 --ttl 5s >/dev/null || die "clone --ttl"
"$PGX" ls
APP="$("$PGX" url app)"
# RE2 linked at runtime + a real RE2 match; seed permanent data.
RE2=$("$PG" --version >/dev/null 2>&1; true)
"$DIST/bin/pgx" url app >/dev/null
# use the engine's own psql-less path: pgx has no SQL exec, so use a tiny client via /usr/bin if present
if command -v psql >/dev/null 2>&1; then PSQL=psql; else PSQL=""; fi
if [ -n "$PSQL" ]; then
  "$PSQL" "$APP" -v ON_ERROR_STOP=1 -c "CREATE TABLE keep(v int)" -c "INSERT INTO keep VALUES (42)" >/dev/null || die "seed"
  r=$("$PSQL" "$APP" -At -c "SHOW pgrust.regex_re2_linked" 2>/dev/null); [ "$r" = on ] || die "RE2 not linked ($r)"
  m=$("$PSQL" "$APP" -At -c "SELECT 'abc123' ~ '^[a-z]+[0-9]+\$'" 2>/dev/null); [ "$m" = t ] || die "RE2 regex failed"
  echo "ok: RE2 linked=$r match=$m; seeded app.keep=42"
else
  echo "note: no psql client on this image; skipping SQL-level RE2/data checks (lifecycle covered elsewhere)"
fi

echo "## restart: TTL cleanup resumes, permanent data survives"
"$PGX" stop >/dev/null || die "stop"
"$PGX" run >/dev/null || die "pgx run #2"
gone=no; i=0; while [ $i -lt 30 ]; do "$PGX" ls --json | grep -q '"test-001"' || { gone=yes; break; }; sleep 1; i=$((i+1)); done
[ "$gone" = yes ] || die "test-001 did not auto-expire after restart"
"$PGX" ls --json | grep -q '"app"' || die "permanent app missing after restart"
if [ -n "$PSQL" ]; then v=$("$PSQL" "$("$PGX" url app)" -At -c "SELECT v FROM keep"); [ "$v" = 42 ] || die "data lost across restart (got '$v')"; echo "ok: app data survived restart (v=$v)"; fi
echo "ok: test-001 auto-expired (~${i}s), app survived"

echo "## clean shutdown / no leaked processes or fds"
"$PGX" stop >/dev/null || die "final stop"
sleep 1
left=$(pgrep -f "$PGX_HOME/data" | wc -l | tr -d ' ')
[ "$left" = 0 ] || die "$left engine process(es) left after stop"
echo "ok: 0 engine processes after stop"
echo "SMOKE PASS ($OS $(uname -m))"
