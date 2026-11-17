#!/bin/sh
# End-to-end test of the npm wrapper (dist/npm, published as `pgboxdb`) against a
# REAL assembled PGBoxDB distribution served over local HTTP as a release would be:
# `node dist/npm/bin/pgboxdb.js` with no arguments must download + verify the
# tarball, cache it, start PGBoxDB and print the `app` connection URL; further
# invocations must reuse the cache and pass every CLI command through.
#
#   PGBOXDB_TARBALL_DIR=<dir with pgboxdb-<v>-<os>-<arch>.tar.gz(.sha256)> dist/test/npm_test.sh
#   (without PGBOXDB_TARBALL_DIR it builds a debug pgboxdb and assembles one; needs
#    PG18_BINDIR or initdb on PATH, and target/release/postgres)
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pgboxdb-npm-test.XXXXXX")"
SRV_PID=""
cleanup() {
  [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true
  PGBOXDB_HOME="$TMP/home" "$TMP/opt/bin/pgboxdb" stop >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

OS="$(uname -s | tr 'A-Z' 'a-z')"; [ "$OS" = darwin ] || OS=linux
ARCH="$(uname -m)"; case "$ARCH" in x86_64|amd64) ARCH=amd64;; aarch64|arm64) ARCH=arm64;; esac

if [ -n "${PGBOXDB_TARBALL_DIR:-}" ]; then
  TGZ="$(find "$PGBOXDB_TARBALL_DIR" -maxdepth 1 -name "pgboxdb-*-$OS-$ARCH.tar.gz" | head -1)"
  [ -n "$TGZ" ] || fail "no pgboxdb-*-$OS-$ARCH.tar.gz in $PGBOXDB_TARBALL_DIR"
  VERSION="$(basename "$TGZ" | sed -n "s/^pgboxdb-\(.*\)-$OS-$ARCH\.tar\.gz$/\1/p")"
else
  VERSION="0.0.0-npm-test"
  ( cd "$ROOT" && CC=/usr/bin/cc cargo build -q -p pgboxdb ) || fail "cargo build -p pgboxdb"
  "$ROOT/dist/assemble.sh" --out "$TMP/stage" --version "$VERSION" \
    --pgboxdb "$ROOT/target/debug/pgboxdb" ${PG18_BINDIR:+--pg-bindir "$PG18_BINDIR"} >/dev/null || fail "assemble"
  TGZ="$TMP/stage/pgboxdb-$VERSION-$OS-$ARCH.tar.gz"
fi
NAME="pgboxdb-$VERSION-$OS-$ARCH"
[ -f "$TGZ" ] && [ -f "$TGZ.sha256" ] || fail "missing $TGZ or its .sha256"

# Release layout as GitHub serves downloads: <base>/v<version>/<name>.tar.gz
mkdir -p "$TMP/srv/v$VERSION"
cp "$TGZ" "$TGZ.sha256" "$TMP/srv/v$VERSION/"
PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
( cd "$TMP/srv" && python3 -m http.server "$PORT" --bind 127.0.0.1 >"$TMP/srv.log" 2>&1 ) &
SRV_PID=$!
sleep 1

export PGBOXDB_VERSION="$VERSION" PGBOXDB_BASE_URL="http://127.0.0.1:$PORT" PGBOXDB_INSTALL_DIR="$TMP/opt"
export PGBOXDB_HOME="$TMP/home"
# No system postgres/psql on PATH: the wrapper and PGBoxDB must not need them.
NODE="$(command -v node)"; PATH="$(dirname "$NODE"):/usr/bin:/bin:/usr/sbin:/sbin"; export PATH
PGBOXDB="$ROOT/dist/npm/bin/pgboxdb.js"

echo "## 1. no arguments == pgboxdb run: downloads, verifies, starts, prints the URL"
node "$PGBOXDB" >"$TMP/run.out" 2>"$TMP/run.err" || { cat "$TMP/run.out" "$TMP/run.err"; fail "pgboxdb (no args) exited non-zero"; }
grep -q "downloading PGBoxDB $VERSION" "$TMP/run.err" || { cat "$TMP/run.err"; fail "did not download"; }
grep -q "PGBoxDB is ready" "$TMP/run.out" || { cat "$TMP/run.out"; fail "no 'PGBoxDB is ready'"; }
URL="$(grep -o 'postgres://[^ ]*' "$TMP/run.out" | head -1)"
[ -n "$URL" ] || fail "no connection URL printed"
[ -x "$TMP/opt/bin/pgboxdb" ] || fail "distribution not cached under PGBOXDB_INSTALL_DIR"
ok "started; URL $(echo "$URL" | sed 's/:[^:@]*@/:***@/')"

echo "## 2. commands pass through, --json works, cache is reused (no second download)"
node "$PGBOXDB" --json ls >"$TMP/ls.json" 2>"$TMP/ls.err" || fail "pgboxdb --json ls"
grep -q '"app"' "$TMP/ls.json" || { cat "$TMP/ls.json"; fail "ls --json lacks app"; }
[ ! -s "$TMP/ls.err" ] || { cat "$TMP/ls.err"; fail "cache hit still wrote to stderr"; }
[ "$(grep -c "$NAME.tar.gz HTTP" "$TMP/srv.log" || true)" = 1 ] || { cat "$TMP/srv.log"; fail "tarball fetched more than once"; }
node "$PGBOXDB" version | grep -q "$VERSION" || fail "pgboxdb version != $VERSION"
node "$PGBOXDB" url app | grep -q '^postgres://' || fail "pgboxdb url app"
ok "ls/version/url pass through; one download total"

echo "## 3. exit codes propagate (drop of a missing database -> 5)"
set +e; node "$PGBOXDB" drop no-such-db >/dev/null 2>&1; rc=$?; set -e
[ "$rc" = 5 ] || fail "expected exit 5 for a missing database, got $rc"
ok "exit code 5 propagated"

echo "## 4. stop through the wrapper"
node "$PGBOXDB" stop >/dev/null || fail "pgboxdb stop"
set +e; node "$PGBOXDB" status >/dev/null 2>&1; rc=$?; set -e
[ "$rc" = 3 ] || fail "expected status exit 3 after stop, got $rc"
echo "PASS: npm wrapper downloads, verifies, caches, runs PGBoxDB and passes commands through ($OS $ARCH)"
