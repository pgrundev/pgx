#!/bin/sh
# Resolve and check the version a release workflow run builds.
#
#   dist/release/version.sh <event_name> <ref_name> [<dispatch_version>]
#
# Prints the version (without a leading v) on stdout, or exits 1 with an
# error on stderr.
#
# * push of tag vX.Y.Z[-pre]: the version is the tag without the v. It must
#   equal the version in crates/bin/pgboxdb/Cargo.toml and
#   dist/npm/package.json, so a tag never ships a binary or npm package
#   that reports another version.
# * workflow_dispatch: the version is the input (ref_name is the branch
#   then, never part of the version). Only the format is checked: a manual
#   run builds artifacts for testing and never creates a release.
#
# ROOT can be overridden (tests).
set -eu
ROOT="${ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
die() { echo "release version: $*" >&2; exit 1; }

[ $# -ge 2 ] || die "usage: version.sh <event_name> <ref_name> [<dispatch_version>]"
EVENT="$1"; REF="$2"; INPUT="${3:-}"

SEMVER='^[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*(-[0-9A-Za-z.-]+)?$'

case "$EVENT" in
  push)
    case "$REF" in v*) ;; *) die "a push release needs a tag v<version>, got '$REF'";; esac
    [ -z "$INPUT" ] || die "a tag push takes no version input"
    VERSION="${REF#v}"
    ;;
  workflow_dispatch)
    [ -n "$INPUT" ] || die "workflow_dispatch needs the version input"
    VERSION="${INPUT#v}"
    ;;
  *) die "unsupported event '$EVENT'" ;;
esac

printf '%s\n' "$VERSION" | grep -Eq "$SEMVER" || die "'$VERSION' is not a version (X.Y.Z or X.Y.Z-pre)"

if [ "$EVENT" = push ]; then
  CRATE="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/crates/bin/pgboxdb/Cargo.toml" | head -1)"
  NPM="$(sed -n 's/^ *"version": *"\(.*\)",*$/\1/p' "$ROOT/dist/npm/package.json" | head -1)"
  [ "$CRATE" = "$VERSION" ] || die "tag $REF does not match crates/bin/pgboxdb/Cargo.toml version '$CRATE'"
  [ "$NPM" = "$VERSION" ] || die "tag $REF does not match dist/npm/package.json version '$NPM'"
fi

printf '%s\n' "$VERSION"
