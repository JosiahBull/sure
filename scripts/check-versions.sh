#!/usr/bin/env bash
# Assert the workspace version is declared consistently in the Rust and Node manifests,
# and — when running on a tag — that the tag (vX.Y.Z) matches. Pure text parsing, so it
# needs no toolchain. Run by the Checks workflow (.github/workflows/checks.yml); on a
# tag build the Release workflow relies on this to reject a mismatched tag.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "::error::$*" >&2; exit 1; }

# [workspace.package] version = "x.y.z"  (read the block, print the version line only)
cargo_version=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p;}' Cargo.toml)
# Root package.json "version": "x.y.z"
pkg_version=$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' package.json | head -n1)

[ -n "$cargo_version" ] || fail "could not parse version from Cargo.toml"
[ -n "$pkg_version" ]   || fail "could not parse version from package.json"

echo "Cargo.toml   : $cargo_version"
echo "package.json : $pkg_version"

[ "$cargo_version" = "$pkg_version" ] \
  || fail "version mismatch: Cargo.toml ($cargo_version) != package.json ($pkg_version)"

# On a tag build, require the tag to match the declared version.
if [ "${GITHUB_REF_TYPE:-}" = "tag" ]; then
  tag_version="${GITHUB_REF_NAME#v}"
  [ "$tag_version" = "$cargo_version" ] \
    || fail "tag ${GITHUB_REF_NAME} does not match declared version $cargo_version"
  echo "tag          : ${GITHUB_REF_NAME} (matches)"
fi

echo "versions OK"
