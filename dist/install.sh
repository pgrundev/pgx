#!/bin/sh
# PGBoxDB installer. Downloads the release tarball for this OS and CPU, verifies
# its SHA-256, and installs it under $PGBOXDB_INSTALL_DIR, linking `pgboxdb` into
# $PGBOXDB_BIN_DIR. No root, no system PostgreSQL.
#
#   curl -fsSL https://pgboxdb.com/install | sh
#   PGBOXDB_VERSION=0.2.0 PGBOXDB_BIN_DIR=~/.local/bin sh install.sh
#
# Environment:
#   PGBOXDB_VERSION       version to install (default: latest release)
#   PGBOXDB_INSTALL_DIR   where the distribution tree goes (default: ~/.pgboxdb/dist/<version>)
#   PGBOXDB_BIN_DIR       where the `pgboxdb` symlink goes     (default: ~/.local/bin)
#   PGBOXDB_BASE_URL      release download base            (default: the GitHub releases URL)
set -eu

REPO="pgrundev/pgboxdb"
BASE_URL="${PGBOXDB_BASE_URL:-https://github.com/$REPO/releases/download}"
# GitHub REST base for release discovery (overridable so the installer is
# testable against a local server).
API_URL="${PGBOXDB_API_URL:-https://api.github.com/repos/$REPO}"
BIN_DIR="${PGBOXDB_BIN_DIR:-$HOME/.local/bin}"

err() { echo "pgboxdb installer: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Platform.
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Linux) os=linux ;;
  Darwin) os=darwin ;;
  *) err "unsupported operating system: $os (PGBoxDB supports Linux and macOS)" ;;
esac
case "$arch" in
  x86_64|amd64) arch=amd64 ;;
  aarch64|arm64) arch=arm64 ;;
  *) err "unsupported CPU architecture: $arch (PGBoxDB supports amd64 and arm64)" ;;
esac
if [ "$os" = darwin ] && [ "$arch" = amd64 ]; then
  err "macOS on Intel is not a supported target; supported: darwin-arm64, linux-amd64, linux-arm64"
fi

# Downloader.
if have curl; then dl() { curl -fsSL "$1" -o "$2"; }; dlout() { curl -fsSL "$1"; }
elif have wget; then dl() { wget -qO "$2" "$1"; }; dlout() { wget -qO- "$1"; }
else err "need curl or wget"; fi

version="${PGBOXDB_VERSION:-}"
if [ -z "$version" ]; then
  # `/releases/latest` EXCLUDES prereleases, so during the beta it would miss
  # e.g. v0.1.0-beta.1. Use the releases list instead (newest first; still
  # excludes drafts) and take the most recent published release's tag. GitHub
  # returns pretty-printed JSON, so the first "tag_name" line is the newest.
  version="$(dlout "$API_URL/releases?per_page=30" | grep -m1 '"tag_name"' | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p')"
  [ -n "$version" ] || err "could not find a published release to install; set PGBOXDB_VERSION (none published yet?)"
fi
version="${version#v}"
install_dir="${PGBOXDB_INSTALL_DIR:-$HOME/.pgboxdb/dist/$version}"
# A data home migrated by `pgboxdb migrate` carries the previous distributions
# under dist/<version>: never replace (delete) one.
[ -f "$install_dir/share/pgx/dist.json" ] && err "$install_dir holds a distribution installed before the rename to PGBoxDB; refusing to replace it (set PGBOXDB_INSTALL_DIR to another directory, or move it aside yourself)"

name="pgboxdb-$version-$os-$arch"
url="$BASE_URL/v$version/$name.tar.gz"
sum_url="$url.sha256"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/pgboxdb-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM
echo "pgboxdb installer: downloading $name ..."
dl "$url" "$tmp/$name.tar.gz" || err "download failed: $url"
dl "$sum_url" "$tmp/$name.tar.gz.sha256" || err "checksum download failed: $sum_url"

echo "pgboxdb installer: verifying checksum ..."
expected="$(awk '{print $1}' "$tmp/$name.tar.gz.sha256")"
if have sha256sum; then actual="$(sha256sum "$tmp/$name.tar.gz" | awk '{print $1}')"
elif have shasum; then actual="$(shasum -a 256 "$tmp/$name.tar.gz" | awk '{print $1}')"
else err "need sha256sum or shasum to verify the download"; fi
[ -n "$expected" ] || err "empty checksum file"
[ "$expected" = "$actual" ] || err "checksum mismatch (expected $expected, got $actual); refusing to install"

echo "pgboxdb installer: extracting ..."
tar -xzf "$tmp/$name.tar.gz" -C "$tmp" || err "extract failed"
[ -x "$tmp/$name/bin/pgboxdb" ] || err "archive does not contain bin/pgboxdb"

# Install atomically: into <dir>.partial then rename.
mkdir -p "$(dirname "$install_dir")"
rm -rf "$install_dir.partial"
cp -R "$tmp/$name" "$install_dir.partial"
# Move any existing install aside first, then rename the new one into place, so
# an interruption can never leave no install at all. Remove the old copy last.
if [ -e "$install_dir" ]; then
  rm -rf "$install_dir.old"
  mv "$install_dir" "$install_dir.old"
fi
mv "$install_dir.partial" "$install_dir"
rm -rf "$install_dir.old" 2>/dev/null || true

mkdir -p "$BIN_DIR"
ln -sf "$install_dir/bin/pgboxdb" "$BIN_DIR/pgboxdb"

# Verify.
out="$("$install_dir/bin/pgboxdb" version 2>&1)" || err "installed, but 'pgboxdb version' failed: $out"
echo "pgboxdb installer: installed $out"
echo "pgboxdb installer: linked $BIN_DIR/pgboxdb -> $install_dir/bin/pgboxdb"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "pgboxdb installer: add $BIN_DIR to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
echo "pgboxdb installer: next: pgboxdb run"
