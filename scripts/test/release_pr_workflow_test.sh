#!/usr/bin/env bash
# release_pr_workflow_test.sh — static validation of the git-cliff front half:
# .github/workflows/release-pr.yml + the scripts/release-pr.sh /
# scripts/release-tag.sh job bodies it calls + cliff.toml.
#
# The live flow needs the RELEASE_PLZ_TOKEN PAT and a push to main, so it can't
# run offline. This asserts the invariants the auto-release contract depends on:
# the workflow triggers on push to main and delegates each job body to its
# script, the scripts compute the bump with git-cliff and prepend CHANGELOG.md
# (never regenerating history), the workflow uses the PAT (never the default
# token) so the pushed tag actually fires release.yml, keeps least-privilege
# perms, and cliff.toml keeps the Keep-a-Changelog shape that
# scripts/changelog-extract.sh parses. Uses grep (dependency-free); if tomllib
# is importable it additionally asserts cliff.toml parses. The scripts'
# runtime behavior is covered by scripts/test/release_pr_script_test.sh.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-pr.yml"
PR_SH="scripts/release-pr.sh"
TAG_SH="scripts/release-tag.sh"
TOML="cliff.toml"
fail=0

for f in "$WF" "$PR_SH" "$TAG_SH" "$TOML"; do
  if [[ ! -f "$f" ]]; then
    echo "FAIL: $f not found" >&2
    exit 1
  fi
done

# want <desc> <file> <pattern> — assert a pattern is PRESENT.
want() {
  local desc="$1" file="$2" pat="$3"
  if grep -Eq -- "$pat" "$file"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in $file: $pat" >&2
    fail=1
  fi
}

# reject <desc> <file> <pattern> — assert a pattern is ABSENT.
reject() {
  local desc="$1" file="$2" pat="$3"
  if grep -Eq -- "$pat" "$file"; then
    echo "FAIL: $desc — pattern found in $file but must not be: $pat" >&2
    fail=1
  else
    echo "ok: $desc"
  fi
}

# --- workflow: trigger ---
want "triggers on push to main"          "$WF" "branches: \[main\]"
# --- workflow: kill-switch (inert until an operator opts in) ---
# Both jobs gate on the RELEASE_AUTOMATION_ENABLED repo variable so a merge
# cannot immediately open release PRs / cut a tag until it is set to 'true'.
want "gated on RELEASE_AUTOMATION_ENABLED" "$WF" "vars\.RELEASE_AUTOMATION_ENABLED == 'true'"
n_gates="$(grep -Ec -- "if: .*vars\.RELEASE_AUTOMATION_ENABLED == 'true'" "$WF")"
if [[ "$n_gates" == "2" ]]; then
  echo "ok: both jobs gated on the kill-switch"
else
  echo "FAIL: expected the kill-switch on both jobs, found $n_gates" >&2
  fail=1
fi
# --- workflow: thin steps that delegate to the tested scripts ---
want "pr job calls release-pr.sh"        "$WF" "run: bash scripts/release-pr\.sh"
want "tag job calls release-tag.sh"      "$WF" "run: bash scripts/release-tag\.sh"
want "pins the git-cliff version"        "$WF" "GIT_CLIFF_VERSION"
# --- scripts: git-cliff drives the bump and the changelog ---
want "computes bump via git-cliff"       "$PR_SH" "git-cliff --bumped-version"
want "prepends CHANGELOG (no rewrite)"   "$PR_SH" "git-cliff --unreleased --bump --prepend CHANGELOG\.md"
# --- scripts: the two-push contract ---
# release-pr skips when main carries an untagged bump (that push belongs to
# release-tag); release-tag tags exactly that state.
want "untagged-bump guard in pr script"  "$PR_SH" "release-tag owns this push"
want "no-releasable-commits guard"       "$PR_SH" "no releasable commits"
want "pr branch force-pushed"            "$PR_SH" "git push --force origin release-pr"
want "tag pushed as annotated vX.Y.Z"    "$TAG_SH" "git tag -a \"v\\\$\{current\}\""
# --- workflow: the token MUST be the PAT, never the default token ---
# A tag pushed under the built-in GITHUB_TOKEN is suppressed by GitHub's
# anti-recursion rule, so release.yml would never fire.
want "checkout auths with the PAT"       "$WF" "token: \\\$\{\{ secrets\.RELEASE_PLZ_TOKEN \}\}"
want "gh auths with the PAT"             "$WF" "GH_TOKEN: \\\$\{\{ secrets\.RELEASE_PLZ_TOKEN \}\}"
reject "never uses the default token"    "$WF" "\\\$\{\{ secrets\.GITHUB_TOKEN \}\}"
reject "never uses github.token"         "$WF" "\\\$\{\{ github\.token \}\}"
# --- workflow: least privilege + full history ---
want "least-privilege default perms"     "$WF" "^permissions:"
want "contents: read default"            "$WF" "contents: read"
reject "tag job takes no pr scope"       "$WF" "pull-requests: read"
want "pr job grants pr: write"           "$WF" "pull-requests: write"
want "checkout with full history"        "$WF" "fetch-depth: 0"
want "pr job serializes on the ref"      "$WF" "cancel-in-progress: false"

# --- cliff.toml: Keep-a-Changelog shape scripts/changelog-extract.sh parses ---
want "header keeps the Unreleased slot"  "$TOML" '^## \[Unreleased\]'
want "version heading has no v prefix"   "$TOML" 'trim_start_matches\(pat="v"\)'
want "conventional commits only"         "$TOML" '^conventional_commits = true'
want "releases keyed to vX.Y.Z tags"     "$TOML" 'tag_pattern = "\^v\[0-9\]'
# --- cliff.toml: the commit -> section mapping the changelog contract uses ---
want "feat -> Added"                     "$TOML" '\{ message = "\^feat", group = "Added" \}'
want "fix -> Fixed"                      "$TOML" '\{ message = "\^fix", group = "Fixed" \}'
want "docs -> Documentation"             "$TOML" '\{ message = "\^docs", group = "Documentation" \}'
want "chore skipped"                     "$TOML" '\{ message = "\^chore", skip = true \}'

# --- optional: deep parse if tomllib is available (Python 3.11+) ---
if python3 -c 'import tomllib' >/dev/null 2>&1; then
  if python3 - "$TOML" <<'PY'
import sys, tomllib
cfg = tomllib.load(open(sys.argv[1], "rb"))
git = cfg["git"]
assert git["conventional_commits"] is True, "conventional_commits must be true"
assert git["tag_pattern"].startswith("^v[0-9]"), git["tag_pattern"]
groups = {p.get("message"): p.get("group") for p in git["commit_parsers"] if "group" in p}
assert groups.get("^feat") == "Added" and groups.get("^fix") == "Fixed", groups
skips = {p["message"] for p in git["commit_parsers"] if p.get("skip")}
assert {"^chore", "^ci", "^test", "^build"} <= skips, skips
ch = cfg["changelog"]
assert "## [Unreleased]" in ch["header"], "header must keep the Unreleased slot"
assert 'trim_start_matches(pat="v")' in ch["body"], "heading must drop the v prefix"
bump = cfg["bump"]
assert bump["features_always_bump_minor"] is True
assert bump["breaking_always_bump_major"] is False, "0.x: breaking bumps minor"
print("ok: tomllib deep-check (conventional commits, vX.Y.Z tags, section mapping, 0.x bump rules)")
PY
  then :; else
    echo "FAIL: tomllib deep-check failed" >&2
    fail=1
  fi
else
  echo "# tomllib unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_pr_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_pr_workflow_test: all passed"
