#!/usr/bin/env bash
# Installs an Eclipse jdtls milestone and prints its bin directory (the CI workflow appends it to
# $GITHUB_PATH). Usage: tests/ci/install_jdtls.sh <version> <tarball> <sha256> <dest>
# A destination that already holds bin/jdtls (a restored cache) is reused as is.
# Requires: bash (3.2+), curl, tar, sha256sum or shasum. Linux and macOS.
set -euo pipefail

[ "$#" -eq 4 ] || { echo "usage: $0 <version> <tarball> <sha256> <dest>" >&2; exit 2; }
version="$1"; tarball="$2"; sha="$3"; dest="$4"

if [ ! -x "$dest/bin/jdtls" ]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL --retry 3 -o "$tmp/$tarball" "https://download.eclipse.org/jdtls/milestones/$version/$tarball"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$tarball" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$tmp/$tarball" | cut -d' ' -f1)"
  fi
  [ "$actual" = "$sha" ] || { echo "jdtls checksum mismatch: expected $sha, got $actual" >&2; exit 1; }
  mkdir -p "$dest"
  tar -xzf "$tmp/$tarball" -C "$dest"
  [ -x "$dest/bin/jdtls" ] || { echo "jdtls launcher missing in $dest/bin" >&2; exit 1; }
fi
echo "$dest/bin"
