#!/usr/bin/env bash
# commit-homebrew-formula.sh — render Formula/toolu-runner.rb for a release
# into a Falconiere/homebrew-tap checkout and commit it there. The one formula
# recipe for release-homebrew.yml and scripts/deploy.sh; each caller pushes
# with its own credentials (an App token in CI, the user's git auth locally).
#
# An unchanged formula is not an error: nothing is committed and it exits 0,
# so the caller's push is a no-op.
#
# Usage:
#   commit-homebrew-formula.sh <tag> <sha256sums_path> <tap_dir>
#
# Exits 2 on a usage error; generate-homebrew-formula.sh's exit 1 (a target
# missing from SHA256SUMS) passes through.
set -euo pipefail

tag="${1:-}"
sums="${2:-}"
tap="${3:-}"

if [[ -z "$tag" || ! -f "$sums" || ! -d "$tap/.git" ]]; then
  echo "commit-homebrew-formula: usage: commit-homebrew-formula.sh <tag> <sha256sums_path> <tap_dir>" >&2
  exit 2
fi

mkdir -p "$tap/Formula"
bash "$(dirname "$0")/generate-homebrew-formula.sh" "$tag" "$sums" >"$tap/Formula/toolu-runner.rb"
# Stage BEFORE comparing. On a first release the formula does not yet exist
# in the tap, so it is untracked — and `git diff` ignores untracked files,
# reporting "no changes". `--cached` compares the index against HEAD, which
# sees a new file as an addition and an unchanged formula as a no-op.
git -C "$tap" add Formula/toolu-runner.rb
if git -C "$tap" diff --cached --quiet -- Formula/toolu-runner.rb; then
  echo "commit-homebrew-formula: formula unchanged, nothing to commit"
  exit 0
fi
git -C "$tap" commit -q -m "toolu-runner ${tag}"
echo "commit-homebrew-formula: committed toolu-runner ${tag}"
