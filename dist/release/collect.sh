#!/bin/sh
# Prepare the assets of a draft release from the downloaded build artifacts.
#
#   dist/release/collect.sh <artifacts_dir> <version> <checksums_out>
#
# <artifacts_dir> is what actions/download-artifact produced: one directory
# per build job (pgboxdb-<target>/) holding pgboxdb-<version>-<target>.tar.gz
# and its .sha256.
#
# 1. Drops the experimental darwin-amd64 build. It is an actions artifact
#    only, never a release asset: `npx pgboxdb` and install.sh would pick it
#    up blindly before it has been validated.
# 2. Requires every supported target's tarball and .sha256, and verifies
#    each checksum. A release with a missing asset is refused here, not
#    discovered after publishing (v0.1.3's draft had no assets at all).
# 3. Writes checksums.txt, one line per supported target, sorted.
set -eu
die() { echo "release collect: $*" >&2; exit 1; }
[ $# -eq 3 ] || die "usage: collect.sh <artifacts_dir> <version> <checksums_out>"
DIR="$1"; VERSION="$2"; OUT="$3"
TARGETS="darwin-arm64 linux-amd64 linux-arm64"

[ -d "$DIR" ] || die "no artifacts directory '$DIR'"
rm -rf "$DIR/pgboxdb-darwin-amd64"

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

: > "$OUT.tmp"
for t in $TARGETS; do
  name="pgboxdb-$VERSION-$t.tar.gz"
  tgz="$DIR/pgboxdb-$t/$name"
  [ -f "$tgz" ] || die "missing $tgz"
  [ -f "$tgz.sha256" ] || die "missing $tgz.sha256"
  want="$(cut -d' ' -f1 < "$tgz.sha256")"
  file="$(sed 's/^[0-9a-f]* [ *]*//' < "$tgz.sha256")"
  [ "$file" = "$name" ] || die "$tgz.sha256 names '$file', expected '$name'"
  got="$(sha "$tgz")"
  [ "$want" = "$got" ] || die "checksum mismatch for $name"
  cat "$tgz.sha256" >> "$OUT.tmp"
done

# Anything else is unexpected: a stray target or version would otherwise be
# attached by the release step's globs.
allowed() {
  for t in $TARGETS; do
    [ "$1" = "$DIR/pgboxdb-$t/pgboxdb-$VERSION-$t.tar.gz" ] && return 0
    [ "$1" = "$DIR/pgboxdb-$t/pgboxdb-$VERSION-$t.tar.gz.sha256" ] && return 0
  done
  return 1
}
find "$DIR" -type f \( -name '*.tar.gz' -o -name '*.tar.gz.sha256' \) > "$OUT.files"
stray=""
while IFS= read -r f; do
  allowed "$f" || { stray="$f"; break; }
done < "$OUT.files"
rm -f "$OUT.files"
[ -z "$stray" ] || { rm -f "$OUT.tmp"; die "unexpected release file $stray"; }

sort -k2 "$OUT.tmp" > "$OUT" && rm -f "$OUT.tmp"
cat "$OUT"
