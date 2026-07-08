#!/usr/bin/env bash
# assert-version.sh — assert a release tag matches the workspace version.
#
# The release workflow pushes tag vX.Y.Z; the binary's version comes from
# Cargo.toml [workspace.package] version. This guards against tagging a commit
# whose Cargo version disagrees with the tag, so the published release name and
# the binary's --version can never drift apart.
#
# Usage:
#   assert-version.sh <ref_name> [cargo_toml=Cargo.toml]
#     <ref_name>   git tag / ref, e.g. v0.1.0 (leading 'v' optional).
#
# Exit codes:
#   0  tag (minus leading 'v') == [workspace.package] version
#   1  mismatch
#   2  usage / parse error
set -euo pipefail

ref="${1:-}"
cargo_toml="${2:-Cargo.toml}"

if [[ -z "$ref" ]]; then
  echo "assert-version: missing <ref_name> (usage: assert-version.sh <ref_name> [cargo_toml])" >&2
  exit 2
fi
if [[ ! -f "$cargo_toml" ]]; then
  echo "assert-version: cargo manifest '$cargo_toml' not found" >&2
  exit 2
fi

# Extract the first `version = "..."` under the [workspace.package] section only,
# so dependency version keys elsewhere in the file cannot be picked up.
version="$(awk '
  /^\[workspace\.package\]/ { inpkg=1; next }
  /^\[/                     { inpkg=0 }
  inpkg && /^version[[:space:]]*=/ {
    line=$0
    sub(/^version[[:space:]]*=[[:space:]]*"?/, "", line)
    sub(/".*$/, "", line)
    print line
    exit
  }
' "$cargo_toml")"

if [[ -z "$version" ]]; then
  echo "assert-version: no [workspace.package] version found in '$cargo_toml'" >&2
  exit 2
fi

tag_version="${ref#v}"

if [[ "$tag_version" != "$version" ]]; then
  echo "assert-version: tag '$ref' (version '$tag_version') != Cargo version '$version'" >&2
  exit 1
fi

echo "assert-version: OK — tag '$ref' matches Cargo version '$version'"
