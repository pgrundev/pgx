#!/bin/sh
# dist/assemble.sh — build the relocatable PGBoxDB distribution directory and
# tarball described in dist/layout.md.
#
#   dist/assemble.sh --out DIR --version V [--engine PATH] [--pgboxdb PATH]
#                    [--pg-bindir DIR] [--staged-share DIR]
#
#   --engine        the built server binary   (default: target/release/postgres)
#   --pgboxdb           the built CLI binary      (default: target/release/pgboxdb)
#   --pg-bindir     PostgreSQL 18 bin dir with initdb and pg_config; its share
#                   dir supplies timezone, timezonesets and tsearch_data and
#                   initdb produces share/pgboxdb/base-cluster (default: PATH)
#   --staged-share  the contrib files main_main/build.rs stages beside the
#                   engine (default: <engine dir>/share)
#
# Produces DIR/pgboxdb-V-OS-ARCH/ and DIR/pgboxdb-V-OS-ARCH.tar.gz(.sha256).
# Every file in the distribution is a regular file (symlinks dereferenced).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT=""; VERSION=""; ENGINE="$ROOT/target/release/postgres"; PGBOXDB_BIN="$ROOT/target/release/pgboxdb"
PG_BINDIR=""; STAGED=""; TZDIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --engine) ENGINE="$2"; shift 2 ;;
    --pgboxdb) PGBOXDB_BIN="$2"; shift 2 ;;
    --pg-bindir) PG_BINDIR="$2"; shift 2 ;;
    --staged-share) STAGED="$2"; shift 2 ;;
    --tzdir) TZDIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "assemble.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done
die() { echo "assemble.sh: $*" >&2; exit 1; }
[ -n "$OUT" ] || die "--out is required"
[ -n "$VERSION" ] || die "--version is required"
[ -x "$ENGINE" ] || die "engine binary not found or not executable: $ENGINE"
[ -x "$PGBOXDB_BIN" ] || die "pgboxdb binary not found or not executable: $PGBOXDB_BIN"
[ -n "$STAGED" ] || STAGED="$(dirname "$ENGINE")/share"
[ -d "$STAGED/extension" ] || die "staged contrib dir missing: $STAGED/extension (build the engine with cargo so main_main/build.rs stages it)"

if [ -n "$PG_BINDIR" ]; then INITDB="$PG_BINDIR/initdb"; PG_CONFIG="$PG_BINDIR/pg_config"
else INITDB="$(command -v initdb || true)"; PG_CONFIG="$(command -v pg_config || true)"; fi
[ -x "$INITDB" ] || die "initdb not found (give --pg-bindir)"
[ -x "$PG_CONFIG" ] || die "pg_config not found (give --pg-bindir)"
INITDB_VERSION="$("$INITDB" --version | sed -n 's/^initdb (PostgreSQL) \([0-9][0-9.]*\).*/\1/p')"
case "$INITDB_VERSION" in 18.*) ;; *) die "initdb is $INITDB_VERSION; PostgreSQL 18.x is required" ;; esac
PG_SHARE="$("$PG_CONFIG" --sharedir)"
for d in timezonesets tsearch_data; do [ -d "$PG_SHARE/$d" ] || die "PostgreSQL share dir lacks $d: $PG_SHARE"; done
# timezone: under the PG share dir (macOS/Homebrew), else the system zoneinfo
# (Debian/Ubuntu build PostgreSQL --with-system-tzdata=/usr/share/zoneinfo).
if [ -z "$TZDIR" ]; then
  if [ -d "$PG_SHARE/timezone" ]; then TZDIR="$PG_SHARE/timezone"
  elif [ -d /usr/share/zoneinfo ]; then TZDIR=/usr/share/zoneinfo
  else die "no timezone data found (looked in $PG_SHARE/timezone and /usr/share/zoneinfo); pass --tzdir"; fi
fi
[ -d "$TZDIR" ] || die "timezone dir not found: $TZDIR"
[ -f "$TZDIR/UTC" ] || die "timezone dir has no UTC zone: $TZDIR"

case "$(uname -s)" in Darwin) OS=darwin ;; Linux) OS=linux ;; *) die "unsupported OS: $(uname -s)" ;; esac
case "$(uname -m)" in x86_64|amd64) ARCH=amd64 ;; aarch64|arm64) ARCH=arm64 ;; *) die "unsupported CPU: $(uname -m)" ;; esac
NAME="pgboxdb-$VERSION-$OS-$ARCH"
PREFIX="$OUT/$NAME"
rm -rf "$PREFIX" "$PREFIX.partial"
mkdir -p "$PREFIX.partial/bin" "$PREFIX.partial/share/pgboxdb"
P="$PREFIX.partial"

# Binaries.
cp "$ENGINE" "$P/bin/postgres"; cp "$PGBOXDB_BIN" "$P/bin/pgboxdb"; chmod 755 "$P/bin/postgres" "$P/bin/pgboxdb"

# Share trees the engine reads (dereference symlinks: Homebrew's share dir is links).
copy_tree() { mkdir -p "$2"; (cd "$1" && tar -chf - .) | (cd "$2" && tar -xf -); }
copy_tree "$TZDIR" "$P/share/timezone"
for d in timezonesets tsearch_data; do copy_tree "$PG_SHARE/$d" "$P/share/$d"; done
# Vendored contrib extension scripts and tsearch data win over PostgreSQL's.
copy_tree "$STAGED/extension" "$P/share/extension"
[ -d "$STAGED/tsearch_data" ] && copy_tree "$STAGED/tsearch_data" "$P/share/tsearch_data"
# plpgsql is installed in template1 by initdb; its control/script files let
# `CREATE EXTENSION [IF NOT EXISTS] plpgsql` (pg_dump / Rails structure.sql) resolve.
for f in "$PG_SHARE"/extension/plpgsql.control "$PG_SHARE"/extension/plpgsql--*.sql; do
  [ -e "$f" ] && cp -L "$f" "$P/share/extension/"
done

# Base cluster: a stock PostgreSQL 18 initdb, minus the files pgboxdb writes itself.
CLUSTER="$P/share/pgboxdb/base-cluster"
"$INITDB" -D "$CLUSTER" --no-locale --encoding UTF8 -U postgres -A trust >"$OUT/initdb.log" 2>&1 \
  || { cat "$OUT/initdb.log" >&2; die "initdb failed"; }
rm -f "$CLUSTER/postgresql.conf" "$CLUSTER/pg_hba.conf" "$CLUSTER/pg_ident.conf" "$CLUSTER/postmaster.pid" "$CLUSTER/postmaster.opts"
printf '# Do not edit this file manually!\n# It will be overwritten by the ALTER SYSTEM command.\n' > "$CLUSTER/postgresql.auto.conf"
rm -rf "$CLUSTER/pg_stat/"* "$CLUSTER/pg_stat_tmp/"* "$CLUSTER/log"
chmod -R u+rwX,go-rwx "$CLUSTER"

# Licenses and notices.
cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$ROOT/dist/THIRD-PARTY-LICENSES.md" "$P/"
cp "$ROOT/dist/layout.md" "$P/share/pgboxdb/layout.md"

# Metadata.
ENGINE_COMMIT="$(git -C "$ROOT" rev-parse --short=10 HEAD 2>/dev/null || echo unknown)"
if [ -n "$(git -C "$ROOT" status --porcelain 2>/dev/null | grep -v '^??' || true)" ]; then ENGINE_COMMIT="$ENGINE_COMMIT-dirty"; fi
cat > "$P/share/pgboxdb/dist.json" <<JSON
{"pgboxdb_version":"$VERSION","engine_commit":"$ENGINE_COMMIT","postgres_compat":"18.6","initdb_version":"$INITDB_VERSION","tzdata_source":"$TZDIR","os":"$OS","arch":"$ARCH","built_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
JSON

[ "$(find "$P" -type l | wc -l | tr -d ' ')" = 0 ] || die "symlinks survived in $P"
mv "$P" "$PREFIX"

# Tarball with the top-level directory, and its checksum.
(cd "$OUT" && tar -czf "$NAME.tar.gz" "$NAME")
(cd "$OUT" && { command -v sha256sum >/dev/null && sha256sum "$NAME.tar.gz" || shasum -a 256 "$NAME.tar.gz"; } > "$NAME.tar.gz.sha256")
echo "assembled $PREFIX"
echo "tarball   $OUT/$NAME.tar.gz ($(du -h "$OUT/$NAME.tar.gz" | cut -f1))"
cat "$OUT/$NAME.tar.gz.sha256"
