#!/usr/bin/env bash
# release_plz_workflow_test.sh — static validation of the release-plz front half:
# .github/workflows/release-plz.yml + release-plz.toml.
#
# The live flow needs the RELEASE_PLZ_TOKEN PAT and a push to main, so it can't
# run offline. This asserts the invariants the auto-release contract depends on:
# the workflow triggers on push to main, runs both release-plz commands, uses the
# PAT (never the default token) so the pushed tag actually fires release.yml, and
# keeps least-privilege perms; and release-plz.toml never publishes, cuts exactly
# one `vX.Y.Z` tag, locks all three crates to one version_group, and points the
# changelog at the shared root file. Uses grep (dependency-free); if tomllib is
# importable it additionally asserts release-plz.toml parses.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-plz.yml"
TOML="release-plz.toml"
fail=0

for f in "$WF" "$TOML"; do
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
# Both jobs gate on the RELEASE_PLZ_ENABLED repo variable so a merge cannot
# immediately open release PRs / cut a tag until it is set to 'true'.
want "gated on RELEASE_PLZ_ENABLED var"  "$WF" "vars\.RELEASE_PLZ_ENABLED == 'true'"
# Count only the job-level gate lines (`if:`), not the header comment.
n_gates="$(grep -Ec -- "if: .*vars\.RELEASE_PLZ_ENABLED == 'true'" "$WF")"
if [[ "$n_gates" == "2" ]]; then
  echo "ok: both jobs gated on the kill-switch"
else
  echo "FAIL: expected the kill-switch on both jobs, found $n_gates" >&2
  fail=1
fi
# --- workflow: both release-plz commands ---
want "runs the release command"          "$WF" "command: release$"
want "runs the release-pr command"       "$WF" "command: release-pr$"
want "uses release-plz/action"           "$WF" "uses: release-plz/action@v0\.5"
# --- workflow: the token MUST be the PAT, never the default token ---
# A tag pushed under the built-in GITHUB_TOKEN is suppressed by GitHub's
# anti-recursion rule, so release.yml would never fire. Assert the PAT is used
# and the default token is never handed to release-plz.
want "auths release-plz with the PAT"    "$WF" "GITHUB_TOKEN: \\\$\{\{ secrets\.RELEASE_PLZ_TOKEN \}\}"
reject "never uses the default token"    "$WF" "GITHUB_TOKEN: \\\$\{\{ secrets\.GITHUB_TOKEN \}\}"
reject "never uses github.token"         "$WF" "GITHUB_TOKEN: \\\$\{\{ github\.token \}\}"
# --- workflow: least privilege + full history ---
want "least-privilege default perms"     "$WF" "^permissions:"
want "contents: read default"            "$WF" "contents: read"
# The release job only pushes a tag (contents: write); it needs no pull-requests
# scope. Only the release-pr job (which opens the PR) gets pull-requests: write.
reject "release job takes no pr scope"   "$WF" "pull-requests: read"
want "pr job grants pr: write"           "$WF" "pull-requests: write"
want "checkout with full history"        "$WF" "fetch-depth: 0"
want "checkout drops credentials"        "$WF" "persist-credentials: false"
want "pr job serializes on the ref"      "$WF" "cancel-in-progress: false"

# --- release-plz.toml: never publish, GitHub Release owned by release.yml ---
want "never cargo publish"               "$TOML" "^publish = false"
want "versions from git tags"            "$TOML" "^git_only = true"
want "release.yml owns the GH Release"   "$TOML" "^git_release_enable = false"
want "tags disabled workspace-wide"      "$TOML" "^git_tag_enable = false"
# --- release-plz.toml: exactly one unprefixed vX.Y.Z tag (single-tag pattern) ---
want "single tag named v{{ version }}"   "$TOML" 'git_tag_name = "v\{\{ version \}\}"'
n_tag_names="$(grep -Ec -- '^git_tag_name' "$TOML")"
if [[ "$n_tag_names" == "1" ]]; then
  echo "ok: exactly one git_tag_name"
else
  echo "FAIL: expected exactly 1 git_tag_name, found $n_tag_names" >&2
  fail=1
fi
# --- release-plz.toml: all three crates locked to one version_group ---
n_groups="$(grep -Ec -- '^version_group = "toolu-runner"' "$TOML")"
if [[ "$n_groups" == "3" ]]; then
  echo "ok: all three crates share one version_group"
else
  echo "FAIL: expected version_group on all 3 packages, found $n_groups" >&2
  fail=1
fi
# --- release-plz.toml: shared root changelog ---
want "changelog points at root file"     "$TOML" '^changelog_path = "\./CHANGELOG\.md"'

# --- optional: deep parse if tomllib is available (Python 3.11+) ---
if python3 -c 'import tomllib' >/dev/null 2>&1; then
  if python3 - "$TOML" <<'PY'
import sys, tomllib
cfg = tomllib.load(open(sys.argv[1], "rb"))
ws = cfg["workspace"]
assert ws["publish"] is False, "workspace.publish must be false"
assert ws["git_release_enable"] is False, "workspace.git_release_enable must be false"
assert ws["git_tag_enable"] is False, "workspace.git_tag_enable must be false"
pkgs = {p["name"]: p for p in cfg["package"]}
assert set(pkgs) == {"toolu-runner", "shared", "protocol"}, f"packages: {list(pkgs)}"
assert all(p.get("version_group") == "toolu-runner" for p in pkgs.values()), "version_group mismatch"
tr = pkgs["toolu-runner"]
assert tr["git_tag_enable"] is True, "toolu-runner must enable tags"
assert tr["git_tag_name"] == "v{{ version }}", tr.get("git_tag_name")
assert tr["changelog_path"] == "./CHANGELOG.md", tr.get("changelog_path")
# Only toolu-runner re-enables tags: exactly one tag per release.
assert sum(1 for p in pkgs.values() if p.get("git_tag_enable")) == 1, "exactly one package may tag"
print("ok: tomllib deep-check (publish/release/tag off, 3 crates one version_group, single vX.Y.Z tag, root changelog)")
PY
  then :; else
    echo "FAIL: tomllib deep-check failed" >&2
    fail=1
  fi
else
  echo "# tomllib unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_plz_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_plz_workflow_test: all passed"
