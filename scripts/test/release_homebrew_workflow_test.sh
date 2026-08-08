#!/usr/bin/env bash
# release_homebrew_workflow_test.sh — static validation of
# .github/workflows/release-homebrew.yml.
#
# A published-release event can't be exercised offline, so this asserts the
# invariants the homebrew-publish contract depends on: trigger, prerelease
# skip, least-privilege permissions, checksum download, formula generation,
# App-token mint, and a guarded push to the external tap. Grep tier
# (dependency-free); if PyYAML is importable it additionally asserts the
# file parses.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-homebrew.yml"
fail=0

if [[ ! -f "$WF" ]]; then
  echo "FAIL: $WF not found" >&2
  exit 1
fi

want() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found: $pat" >&2
    fail=1
  fi
}

# Asserts a pattern is ABSENT. Guards against a regression re-introducing a
# construct, which `want` cannot express.
reject() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "FAIL: $desc — pattern found but must not be: $pat" >&2
    fail=1
  else
    echo "ok: $desc"
  fi
}

# Chained from release.yml, NOT `on: release: [published]` — a release created
# by a workflow step using the default GITHUB_TOKEN emits no `release` event,
# so an event-triggered version of this workflow could never fire.
want "callable as a reusable workflow"    "^  workflow_call:"
# Declared, not inherited: the caller passes exactly these two App secrets.
# Anchored to each secret block — a bare `required: true` match would also be
# satisfied by that key appearing under some unrelated secret, or in a comment.
for secret in HOMEBREW_APP_ID HOMEBREW_APP_PRIVATE_KEY; do
  if awk -v s="$secret" '
    $0 ~ "^      " s ":[[:space:]]*$" { inblock = 1; next }
    inblock && /^      [^[:space:]]/          { inblock = 0 }
    inblock && /^        required:[[:space:]]+true[[:space:]]*$/ { found = 1 }
    END { exit !found }
  ' "$WF"; then
    echo "ok: $secret declared required under workflow_call.secrets"
  else
    echo "FAIL: no 'required: true' inside the $secret secret block" >&2
    fail=1
  fi
done
want "skips prereleases"                  "!contains\(github\.ref_name, '-'\)"
want "reads the tag from the caller"      "TAG: \\\$\{\{ github\.ref_name \}\}"
# `workflow_call` being present does not mean `release` is absent — a file may
# declare both, and the event-triggered copy would be just as dead. Scoped to
# the top-level `on:` block: `jobs:` children share the same 2-space indent, so
# a bare `^  release:` would also fail a job legitimately named `release`.
if awk '
  /^on:[[:space:]]*$/        { in_on = 1; next }
  in_on && /^[^[:space:]]/   { in_on = 0 }
  in_on && /^  release:/     { found = 1 }
  END { exit !found }
' "$WF"; then
  echo "FAIL: a 'release:' trigger is declared — this workflow must be chained, not event-triggered" >&2
  fail=1
else
  echo "ok: not event-triggered"
fi
# Under workflow_call there is no `release` event payload. Matches any
# non-comment line, not just `${{ }}` — `if:` accepts a bare expression without
# the braces, which an expression-only pattern would miss. Header comments,
# which start with `#`, stay legal.
reject "no release-event payload reads"   '^[^#]*github\.event\.release'
reject "caller owns concurrency"          "^concurrency:"
want "least-privilege permissions"        "^permissions:"
want "contents: read only"                "contents: read"
want "downloads SHA256SUMS from release"  "gh release download"
want "generates the formula via script"   "generate-homebrew-formula\.sh"
# App install token — same pattern as release-pr.yml / comemory / git-better.
# Reject a long-lived PAT so the expired-HOMEBREW_TAP_TOKEN failure cannot return.
want "mints an App token"                 "uses: actions/create-github-app-token@"
want "scopes the App token to the tap"    "repositories: homebrew-tap"
want "checks out the homebrew-tap repo"   "repository: Falconiere/homebrew-tap"
reject "no long-lived HOMEBREW_TAP_TOKEN" "HOMEBREW_TAP_TOKEN"
want "pushes to the homebrew-tap repo"    "Falconiere/homebrew-tap"
# Must stage before comparing: on a first release the formula is untracked, and
# `git diff` (without --cached) ignores untracked files — it would report "no
# changes", skip the push, and still exit 0. Assert the --cached form, and
# reject the bare one so the regression cannot return.
want "skips an unchanged formula"         "git diff --cached --quiet"
# `git diff --quiet` (no --cached) compares the WORKTREE, which cannot see an
# untracked file — that is the bug. Reject the bare form.
reject "no bare worktree diff"            "git diff --quiet"
if awk '
  /^ *git add Formula\/toolu-runner\.rb$/          { staged = 1 }
  staged && /git diff --cached --quiet/            { ordered = 1 }
  END { exit !ordered }
' "$WF"; then
  echo "ok: stages the formula before diffing it"
else
  echo "FAIL: 'git add' must precede 'git diff --cached' or a new formula is never pushed" >&2
  fail=1
fi
want "commits with the tag in the message" 'git commit -m "toolu-runner \$\{TAG\}"'

if python3 -c 'import yaml' >/dev/null 2>&1; then
  if python3 - "$WF" <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf.get("jobs", {})
assert set(jobs) == {"publish-formula"}, f"jobs: {list(jobs)}"
assert wf["permissions"]["contents"] == "read"
# YAML 1.1 parses the bare `on:` key as the boolean True, not the string "on".
triggers = wf[True]
assert set(triggers) == {"workflow_call"}, f"triggers: {list(triggers)}"
# Exactly the two App secrets, both mandatory. Structural, so a comment or a
# stray `required: true` elsewhere in the file cannot satisfy it.
secrets = triggers["workflow_call"]["secrets"]
assert set(secrets) == {"HOMEBREW_APP_ID", "HOMEBREW_APP_PRIVATE_KEY"}, (
    f"workflow_call secrets: {list(secrets)}"
)
for name in ("HOMEBREW_APP_ID", "HOMEBREW_APP_PRIVATE_KEY"):
    assert secrets[name].get("required") is True, (
        f"{name} must be required, got: {secrets[name].get('required')!r}"
    )
# Mint step must precede the tap checkout and scope to homebrew-tap.
steps = jobs["publish-formula"]["steps"]
mint = next(s for s in steps if s.get("id") == "tap-token")
assert "create-github-app-token@" in mint["uses"]
assert mint["with"]["owner"] == "Falconiere"
assert mint["with"]["repositories"] == "homebrew-tap"
tap_checkout = next(
    s for s in steps
    if s.get("with", {}).get("repository") == "Falconiere/homebrew-tap"
)
assert tap_checkout["with"]["path"] == "tap"
assert "tap-token.outputs.token" in tap_checkout["with"]["token"]
print("ok: PyYAML deep-check (job set + read-only perm + App secrets + mint+checkout tap)")
PY
  then :; else
    echo "FAIL: PyYAML deep-check failed" >&2
    fail=1
  fi
else
  echo "# PyYAML unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_homebrew_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_homebrew_workflow_test: all passed"
