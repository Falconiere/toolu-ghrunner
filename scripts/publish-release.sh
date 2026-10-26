#!/usr/bin/env bash
# publish-release.sh — checksum the release tarballs and create the GitHub
# Release. The one publish recipe for release.yml's `publish` job and
# scripts/deploy.sh.
#
#   - <dist_dir>/SHA256SUMS over every *.tar.gz in it (sha256sum, else
#     shasum -a 256 — same output format)
#   - a tag containing '-' (e.g. v0.2.0-rc.1) is a --prerelease, keeping
#     GitHub's `latest` (which install.sh resolves) stable-only
#   - gh release create <tag> with the toolu-runner + toolu-daemon tarballs and
#     SHA256SUMS; the repo is gh's default (the checkout, or GH_REPO)
#
# Usage:
#   publish-release.sh <tag> <dist_dir> <notes_file> [target_sha]
#     [target_sha]  commit the new tag points at, when the tag does not exist
#                   yet (deploy.sh); release.yml's tag already exists
#
# Exits 2 on a usage error.
set -euo pipefail

tag="${1:-}"
dist="${2:-}"
notes="${3:-}"
target="${4:-}"

if [[ -z "$tag" || -z "$dist" || ! -f "$notes" ]]; then
  echo "publish-release: usage: publish-release.sh <tag> <dist_dir> <notes_file> [target_sha]" >&2
  exit 2
fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}
(cd "$dist" && sha256 -- *.tar.gz >SHA256SUMS)

args=(--title "$tag" --notes-file "$notes")
if [[ "$tag" == *-* ]]; then
  args+=(--prerelease)
fi
if [[ -n "$target" ]]; then
  args+=(--target "$target")
fi

gh release create "$tag" \
  "$dist"/toolu-runner-*.tar.gz "$dist"/toolu-daemon-*.tar.gz "$dist/SHA256SUMS" \
  "${args[@]}"
