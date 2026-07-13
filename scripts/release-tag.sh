#!/usr/bin/env bash
# release-tag.sh — tag a merged version bump.
#
# Body of the release-tag job in .github/workflows/release-pr.yml (the back
# half of the release pipeline; scripts/release-pr.sh is the front half).
# When main's [workspace.package] version has no matching vX.Y.Z tag yet —
# i.e. the release PR just merged — create the annotated tag and push it.
# The pushed tag (under the RELEASE_PLZ_TOKEN PAT, never the default token)
# fires .github/workflows/release.yml (verify -> build -> GitHub Release ->
# Homebrew). Exits 0 without tagging when the version is already tagged.
#
# Env:
#   DRY_RUN=1  create the local tag but skip `git push` — lets
#              scripts/test/release_pr_script_test.sh assert the tag
#              in a fixture repo without a remote.
set -euo pipefail

dry_run="${DRY_RUN:-0}"

# [workspace.package] version only — f resets on the next section header so a
# `version = ` line in a later section can never win.
current=$(awk '/^\[workspace.package\]/{f=1; next} /^\[/{f=0} f && /^version = /{gsub(/"/,"",$3); print $3; exit}' Cargo.toml)
if git rev-parse -q --verify "refs/tags/v${current}" >/dev/null; then
  echo "v${current} already tagged"
  exit 0
fi
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git tag -a "v${current}" -m "toolu-runner ${current}"
if [[ "${dry_run}" == "1" ]]; then
  echo "DRY_RUN: local tag v${current} created; skipping push"
  exit 0
fi
git push origin "v${current}"
