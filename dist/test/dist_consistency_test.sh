#!/bin/sh
# Every distribution channel uses the same PGBoxDB names: npm package,
# release artifacts, installer, installer Worker, Docker image, CI guards.
# Static checks over the files (no build, no network); the behavioural
# tests (install_test, npm_test, docker_test, release_workflow_test,
# install-worker.test.js) exercise the same names end to end.
#
#   dist/test/dist_consistency_test.sh
# Literal $ in single quotes is intended (the patterns are code); `a && ok || bad` is fine: ok never fails.
# shellcheck disable=SC2016,SC2015
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
FAILS=0; N=0
ok() { N=$((N + 1)); echo "ok   $*"; }
bad() { N=$((N + 1)); FAILS=$((FAILS + 1)); echo "FAIL $*"; }
check() { name="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$name"; else bad "$name"; fi; }
has() { grep -qF -- "$2" "$1"; }

REPO="pgrundev/pgboxdb"
# 1. One repository slug.
check "Cargo.toml repository"          has crates/bin/pgboxdb/Cargo.toml "repository = \"https://github.com/$REPO\""
check "npm package.json repository"    has dist/npm/package.json "github.com/$REPO.git"
check "npm wrapper downloads from $REPO" has dist/npm/lib/pgboxdb.js "const REPO = '$REPO';"
check "install.sh downloads from $REPO" has dist/install.sh "REPO=\"$REPO\""
check "Worker serves install.sh from $REPO" has dist/pgboxdb.com/install-worker.js "const REPO = \"$REPO\";"
for wf in npm-publish publish-plugin; do
  check "$wf.yml runs only in $REPO" has ".github/workflows/$wf.yml" "github.repository == '$REPO'"
done

# 2. One binary name.
grep -A1 -F '[[bin]]' crates/bin/pgboxdb/Cargo.toml | grep -qx 'name = "pgboxdb"' && ok "crate [[bin]] name = pgboxdb" || bad "crate [[bin]] name = pgboxdb"
bins="$(sed -n '/"bin": {/,/}/p' dist/npm/package.json | sed 1d | grep -oE '^ *"[a-z]+": ' | tr -d ' ":' | tr '\n' ' ')"
[ "$bins" = "pgboxdb " ] && ok "npm package has exactly the pgboxdb bin" || bad "npm bins are '$bins'"
check "npm package name is pgboxdb"    grep -q '"name": "pgboxdb"' dist/npm/package.json
check "assemble ships bin/pgboxdb"     has dist/assemble.sh 'bin/pgboxdb'
check "install.sh links pgboxdb"       has dist/install.sh 'ln -sf "$install_dir/bin/pgboxdb" "$BIN_DIR/pgboxdb"'
check "Docker links /usr/local/bin/pgboxdb" has Dockerfile 'ln -s /opt/pgboxdb/bin/pgboxdb /usr/local/bin/pgboxdb'
check "release builds --bin pgboxdb"   has .github/workflows/release.yml '--bin postgres --bin pgboxdb'

# 3. One asset name: pgboxdb-<version>-<os>-<arch>.tar.gz (+ .sha256).
check "assemble names pgboxdb-V-OS-ARCH" has dist/assemble.sh 'NAME="pgboxdb-$VERSION-$OS-$ARCH"'
check "install.sh fetches pgboxdb-V-OS-ARCH" grep -q 'name="pgboxdb-$version-$os-$arch"' dist/install.sh
check "install.sh fetches <url>.sha256" has dist/install.sh 'sum_url="$url.sha256"'
check "npm fetches pgboxdb-V-T.tar.gz"   has dist/npm/lib/pgboxdb.js 'return `pgboxdb-${version}-${target}.tar.gz`;'
check "npm fetches from /v<version>/"    has dist/npm/lib/pgboxdb.js '/v${version}/${assetName(version, target)}'
check "install.sh fetches from /v<version>/" has dist/install.sh 'url="$BASE_URL/v$version/$name.tar.gz"'
check "release collects pgboxdb-V-T.tar.gz" has dist/release/collect.sh 'name="pgboxdb-$VERSION-$t.tar.gz"'
check "release uploads per-target artifacts pgboxdb-<target>" has .github/workflows/release.yml 'name: pgboxdb-${{ matrix.target }}'

# 4. One home and install layout.
check "npm installs under ~/.pgboxdb/dist/<v>"  has dist/npm/lib/pgboxdb.js "path.join(home, '.pgboxdb', 'dist', version)"
check "install.sh installs under ~/.pgboxdb/dist/<v>" has dist/install.sh 'install_dir="${PGBOXDB_INSTALL_DIR:-$HOME/.pgboxdb/dist/$version}"'
check "layout file is share/pgboxdb/dist.json" has dist/assemble.sh 'share/pgboxdb'

# 5. Docker image: user, paths, environment.
for s in 'useradd -r -u 10001 pgboxdb' 'VOLUME /var/lib/pgboxdb' 'COPY --from=builder --chown=pgboxdb:pgboxdb /pgboxdb /opt/pgboxdb' \
         'ENV PGBOXDB_HOME=/var/lib/pgboxdb PGBOXDB_RUN_DIR=/var/run/pgboxdb' 'USER pgboxdb' 'ENTRYPOINT ["pgboxdb-entrypoint"]'; do
  check "Dockerfile: $s" has Dockerfile "$s"
done

# 6. Every environment variable the channels read is PGBOXDB_* (or a
#    standard one: HOME, TMPDIR, PATH).
STD='^(HOME|TMPDIR|PATH)$'
for f in dist/npm/lib/pgboxdb.js dist/npm/bin/pgboxdb.js; do
  other="$(grep -oE 'env\.[A-Z][A-Z0-9_]*' "$f" | sed 's/^env\.//' | grep -vE '^PGBOXDB_' | grep -vE "$STD" | sort -u | tr '\n' ' ' || true)"
  [ -z "$other" ] && ok "$f reads PGBOXDB_* only" || bad "$f reads $other"
done
for f in dist/install.sh dist/docker/entrypoint.sh dist/assemble.sh; do
  other="$(grep -oE '\$\{[A-Z][A-Z0-9_]*:?[-+=?]' "$f" | sed 's/^\${//; s/:*[-+=?]$//' | grep -vE '^PGBOXDB_' | grep -vE "$STD" | sort -u | tr '\n' ' ' || true)"
  [ -z "$other" ] && ok "$f reads PGBOXDB_* only" || bad "$f reads $other"
done
other="$(grep -E '^ENV ' Dockerfile | grep -oE '[A-Z][A-Z0-9_]*=' | tr -d = | grep -vE '^(PGBOXDB_|PATH$|CC$)' | sort -u | tr '\n' ' ' || true)"
[ -z "$other" ] && ok "Dockerfile ENV sets PGBOXDB_* only (plus PATH, CC)" || bad "Dockerfile ENV sets $other"

# 7. The installer Worker never serves an install.sh from before the rename.
ref="$(sed -n 's/^INSTALL_REF *= *"\(.*\)"/\1/p' dist/pgboxdb.com/wrangler.toml)"
case "$ref" in
  v0.0.*|v0.1.*|v0.1|v0.0) bad "Worker INSTALL_REF $ref predates PGBoxDB (its install.sh installs the previous product)" ;;
  v[0-9]*.[0-9]*.[0-9]*) ok "Worker INSTALL_REF $ref is a PGBoxDB release tag" ;;
  *) bad "Worker INSTALL_REF '$ref' is not a release tag" ;;
esac
check "Worker route is pgboxdb.com/install" has dist/pgboxdb.com/wrangler.toml 'pattern = "pgboxdb.com/install", zone_name = "pgboxdb.com"'

# 8. No previous-name identifier in a channel, except the reviewed lines that
#    detect an installation from before the rename (no alias is shipped).
ALLOW="share/pgx/dist\\.json|share/pgx|'share', 'pgx'|/var/run/pgx|pgx_version|from before the rename|before the rename"
hits="$(grep -rnE '(^|[^a-z0-9])(pgx|PGX|pgxdb)([^a-z0-9]|$)|pgx\.dev|pgrundev/pgx([^-]|$)' \
  dist/install.sh dist/assemble.sh dist/layout.md dist/npm/lib dist/npm/bin dist/npm/package.json dist/npm/README.md \
  dist/docker Dockerfile dist/pgboxdb.com/install-worker.js dist/pgboxdb.com/wrangler.toml dist/release \
  .github/workflows 2>/dev/null | grep -vE "$ALLOW" || true)"
[ -z "$hits" ] && ok "no previous-name identifier outside the legacy detection lines" || { bad "previous-name identifiers:"; echo "$hits" | sed 's/^/       /'; }

# 9. Versions agree (the release workflow refuses a tag otherwise).
crate="$(sed -n 's/^version *= *"\(.*\)"/\1/p' crates/bin/pgboxdb/Cargo.toml | head -1)"
npm="$(sed -n 's/^ *"version": *"\(.*\)",*$/\1/p' dist/npm/package.json | head -1)"
[ "$crate" = "$npm" ] && ok "crate and npm versions agree ($crate)" || bad "crate $crate vs npm $npm"

# 10. The Worker serves the installer of the version being released.
[ "$ref" = "v$crate" ] && ok "Worker INSTALL_REF is v$crate" || bad "Worker INSTALL_REF $ref is not v$crate (bump it with the version)"

echo "$((N - FAILS))/$N checks passed"
[ "$FAILS" -eq 0 ]
