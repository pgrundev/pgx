#!/bin/sh
# Test for dist/assemble.sh: assembles a distribution into a temp dir and
# checks the layout documented in dist/layout.md.
#
#   PG18_BINDIR=/opt/homebrew/opt/postgresql@18/bin dist/test/assemble_test.sh
#
# Needs: a built engine (target/release/postgres), PostgreSQL 18 initdb and
# pg_config (PG18_BINDIR, or on PATH).
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pgx-assemble-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }

# A stand-in pgx binary: Task 1 only needs *a* file at bin/pgx.
PGX_STUB="$TMP/pgx-stub"; printf '#!/bin/sh\necho stub\n' > "$PGX_STUB"; chmod +x "$PGX_STUB"

"$ROOT/dist/assemble.sh" --out "$TMP/out" --version 0.0.0-test \
  --engine "$ROOT/target/release/postgres" --pgx "$PGX_STUB" \
  ${PG18_BINDIR:+--pg-bindir "$PG18_BINDIR"} >"$TMP/assemble.log" 2>&1 \
  || { cat "$TMP/assemble.log"; fail "assemble.sh exited non-zero"; }

PREFIX="$(find "$TMP/out" -maxdepth 1 -type d -name 'pgx-0.0.0-test-*' | head -1)"
[ -n "$PREFIX" ] || fail "no pgx-0.0.0-test-<os>-<arch> directory under out/"
case "$(basename "$PREFIX")" in
  pgx-0.0.0-test-darwin-arm64|pgx-0.0.0-test-darwin-amd64|pgx-0.0.0-test-linux-amd64|pgx-0.0.0-test-linux-arm64) ;;
  *) fail "unexpected platform suffix: $(basename "$PREFIX")" ;;
esac

for p in bin/pgx bin/postgres share/timezone/UTC share/timezonesets/Default \
         share/tsearch_data/english.stop share/tsearch_data/unaccent.rules \
         share/extension/plpgsql.control share/extension/pgcrypto.control \
         share/pgx/base-cluster/PG_VERSION share/pgx/base-cluster/global/pg_control \
         share/pgx/base-cluster/base/1/PG_VERSION share/pgx/dist.json \
         LICENSE NOTICE THIRD-PARTY-LICENSES.md; do
  [ -e "$PREFIX/$p" ] || fail "missing $p"
done
[ -x "$PREFIX/bin/postgres" ] || fail "bin/postgres not executable"
[ -x "$PREFIX/bin/pgx" ] || fail "bin/pgx not executable"
# No symlinks survive (brew's share dir is symlinks; the dist must be real files).
n="$(find "$PREFIX" -type l | wc -l | tr -d ' ')"; [ "$n" = 0 ] || fail "$n symlinks in the distribution"
# pgx owns these; the base cluster must not carry the build host's copies.
for p in postgresql.conf pg_hba.conf pg_ident.conf postmaster.pid postmaster.opts; do
  [ ! -e "$PREFIX/share/pgx/base-cluster/$p" ] || fail "base-cluster carries $p"
done
grep -q '^18' "$PREFIX/share/pgx/base-cluster/PG_VERSION" || fail "base cluster is not PostgreSQL 18"

python3 - "$PREFIX/share/pgx/dist.json" <<'PY' || fail "dist.json invalid"
import json, sys
d = json.load(open(sys.argv[1]))
for k in ("pgx_version", "engine_commit", "postgres_compat", "initdb_version", "os", "arch", "built_at"):
    assert k in d and d[k], k
assert d["pgx_version"] == "0.0.0-test", d["pgx_version"]
assert d["postgres_compat"] == "18.6", d["postgres_compat"]
assert d["os"] in ("darwin", "linux") and d["arch"] in ("arm64", "amd64")
PY

TARBALL="$TMP/out/$(basename "$PREFIX").tar.gz"
[ -f "$TARBALL" ] || fail "missing tarball $TARBALL"
[ -f "$TARBALL.sha256" ] || fail "missing $TARBALL.sha256"
tar -tzf "$TARBALL" | grep -q "^$(basename "$PREFIX")/bin/postgres$" || fail "tarball lacks bin/postgres under the top-level dir"
( cd "$TMP/out" && { command -v sha256sum >/dev/null && sha256sum -c "$(basename "$TARBALL").sha256" || shasum -a 256 -c "$(basename "$TARBALL").sha256"; } ) >/dev/null || fail "checksum does not verify"
echo "PASS: dist/assemble.sh layout ($(basename "$PREFIX"))"
