#!/usr/bin/env bash
# release-pr.sh — compute the version bump and open/update the release PR.
#
# Body of the release-pr job in .github/workflows/release-pr.yml (the front
# half of the release pipeline; scripts/release-tag.sh is the back half).
# Reads the conventional commits since the last vX.Y.Z tag with git-cliff,
# bumps [workspace.package] version in Cargo.toml, prepends the new section
# to CHANGELOG.md, commits the result to the `release-pr` branch, then
# force-pushes it and opens the PR via `gh` (if one isn't already open).
#
# Two exit-0 guards keep the two-push contract with release-tag:
#   * main carries an untagged bump  -> "release-tag owns this push"
#   * nothing new since the last tag -> "no releasable commits"
#
# Requires: git, git-cliff (workflow pins GIT_CLIFF_VERSION), and in normal
# mode cargo + gh with GH_TOKEN (the RELEASE_PLZ_TOKEN PAT).
#
# Env:
#   DRY_RUN=1  stop after the local commit: skip `cargo update` (no Cargo.lock
#              in the test fixture), `git push`, and `gh pr` — lets
#              scripts/test/release_pr_script_test.sh exercise
#              bump + changelog + commit in a fixture repo without network/gh.
#   GH_TOKEN   auth for `gh pr` (normal mode only).
set -euo pipefail

dry_run="${DRY_RUN:-0}"

# [workspace.package] version only — f resets on the next section header so a
# `version = ` line in a later section can never win.
current=$(awk '/^\[workspace.package\]/{f=1; next} /^\[/{f=0} f && /^version = /{gsub(/"/,"",$3); print $3; exit}' Cargo.toml)
if ! git rev-parse -q --verify "refs/tags/v${current}" >/dev/null; then
  echo "main carries untagged bump v${current} — release-tag owns this push; skipping PR"
  exit 0
fi
next=$(git-cliff --bumped-version)
if [ "${next}" = "v${current}" ]; then
  echo "no releasable commits since v${current}"
  exit 0
fi
next_ver=${next#v}
echo "release PR: v${current} -> ${next}"
# Whole-line match (no interpolated version, so no regex-metacharacter risk):
# the root Cargo.toml has exactly one `^version = ` line — the
# [workspace.package] one every crate inherits. `-i.bak` + rm is the
# GNU/BSD-portable in-place idiom (CI is GNU sed, the local test may be BSD).
sed -i.bak -E "s/^version = \".*\"$/version = \"${next_ver}\"/" Cargo.toml && rm -f Cargo.toml.bak
if [[ "${dry_run}" == "1" ]]; then
  echo "DRY_RUN: skipping cargo update"
else
  cargo update --workspace --quiet
fi
git-cliff --unreleased --bump --prepend CHANGELOG.md -o /dev/null
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git checkout -B release-pr
git add Cargo.toml CHANGELOG.md
if [[ "${dry_run}" != "1" ]]; then
  git add Cargo.lock
fi
git commit -m "chore(release): ${next}"
if [[ "${dry_run}" == "1" ]]; then
  echo "DRY_RUN: local commit created; skipping push and PR"
  exit 0
fi
git push --force origin release-pr
if ! gh pr list --head release-pr --state open --json number --jq '.[0].number' | grep -q .; then
  gh pr create --head release-pr --base main \
    --title "chore(release): ${next}" \
    --body "Automated release PR: bumps the workspace version to ${next_ver} and updates CHANGELOG.md from the conventional commits since v${current}. Merging this PR tags ${next}, which builds and publishes the release."
fi
