#!/usr/bin/env bash
# code_review_workflow_test.sh — static validation of .github/workflows/
# code-review.yml + the .github/code-review-prompt.md checklist it injects.
#
# The live flow needs an OpenRouter key and a real PR, so it can't run offline.
# This pins the invariants the review contract depends on: the reviewer is
# wired to the repo-tuned prompt under merge-ref rules, its budget/round caps
# stay set, and — the regression this file exists for — it does NOT review the
# machine-generated release PR.
#
# Every byte of the `release-pr` branch is written by scripts/release-pr.sh
# (git-cliff CHANGELOG prepend, [workspace.package] version line, cargo
# update), so the reviewer had no human-authored code to judge and produced the
# same two false positives on every release (PR #49): it read the generated
# `## [X.Y.Z]` heading under the permanently empty `## [Unreleased]` slot as a
# hand-written entry, and re-derived the bump against plain SemVer instead of
# cliff.toml's pre-1.0 [bump] policy. The skip is the fix; the prompt assertions
# below are the belt-and-braces half, so a human PR touching CHANGELOG.md can't
# resurrect either reading.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/code-review.yml"
PROMPT=".github/code-review-prompt.md"
PR_SH="scripts/release-pr.sh"
fail=0

for f in "$WF" "$PROMPT" "$PR_SH"; do
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

# --- the release PR is never reviewed ---
want "review job skips the release PR"    "$WF" "github\.head_ref != 'release-pr'"
# The head-repo half is the fork guard: `github.head_ref` carries only a branch
# name and reads identically for a fork PR, so on the name alone anyone could
# fork this public repo, push a branch called `release-pr`, and have their
# review skipped — reported as SUCCESS, since a skipped job passes. Losing this
# half is a silent hole, so pin it separately from the branch test.
want "skip is limited to this repo"       "$WF" \
  "github\.event\.pull_request\.head\.repo\.full_name != github\.repository"
# A job-level `if` (skipped => success) and NOT an on: filter: a workflow
# filtered out of the event never reports, so making Code Review a required
# check would leave the release PR pending forever.
want "skip is a job-level condition"      "$WF" "^ +if: \\\$\{\{ github\.head_ref"
# Anchored to an actual YAML key (indent + key + colon), so the words
# "branches-ignore" appearing in the comment above the skip don't trip it.
if grep -Eq -- "^ *(branches|branches-ignore):" "$WF"; then
  echo "FAIL: skip must be a job-level if, not an on: branch filter" >&2
  fail=1
else
  echo "ok: no on: branch filter (the job-level if owns the skip)"
fi
# The branch name is the contract between the two files — release-pr.sh
# force-pushes it and the workflow keys the skip off it. Drift breaks the skip
# silently, so assert both ends still say `release-pr`.
#
# Right-anchored on purpose: unanchored, these match `release-pr` as a PREFIX,
# so renaming the branch to `release-pr-v2` in release-pr.sh would still pass
# them while breaking the workflow's exact-string skip — precisely the drift
# these two lines exist to catch. `([[:space:]]|$)` pins the whole token.
want "release-pr.sh pushes that branch"   "$PR_SH" \
  "git push --force origin release-pr([[:space:]]|$)"
want "release-pr.sh opens that PR head"   "$PR_SH" \
  "gh pr (list|create) --head release-pr([[:space:]]|$)"

# --- the reviewer stays wired to the repo-tuned checklist ---
want "uses the repo review prompt"        "$WF" "REVIEW_PROMPT_FILE: \.github/code-review-prompt\.md"
want "reads conventions from merge ref"   "$WF" "RULES_REF: merge"
want "keeps the surrender cap"            "$WF" "MAX_ROUNDS:"
want "keeps the raised token cap"         "$WF" "MAX_TOKENS:"
want "excludes fixtures from the diff"    "$WF" "EXCLUDE_GLOBS:"

# --- the prompt cannot re-derive either false positive ---
want "prompt: changelog is generated"     "$PROMPT" "generated, never hand-written"
want "prompt: release heading is sibling" "$PROMPT" "sibling"
want "prompt: no changelog entry asks"    "$PROMPT" "Do not ask for a CHANGELOG entry"
want "prompt: no bump re-derivation"      "$PROMPT" "do not re-derive a"
want "prompt: states the pre-1.0 policy"  "$PROMPT" "breaking change bumps the minor while 0\.x"

# --- optional: deep parse if PyYAML is available ---
if python3 -c 'import yaml' >/dev/null 2>&1; then
  if python3 - "$WF" <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
# PyYAML resolves the bare `on:` key to the boolean True (YAML 1.1).
trigger = wf.get("on", wf.get(True))
assert "pull_request" in trigger, trigger
pr = trigger["pull_request"] or {}
assert "branches" not in pr and "branches-ignore" not in pr, \
    "the skip must be a job-level if, not an on: branch filter"
job = wf["jobs"]["review"]
cond = " ".join((job.get("if") or "").split())
assert cond == (
    "${{ github.head_ref != 'release-pr' "
    "|| github.event.pull_request.head.repo.full_name != github.repository }}"
), cond
print("ok: PyYAML deep-check (pull_request unfiltered, same-repo release-pr skipped)")
PY
  then :; else
    echo "FAIL: PyYAML deep-check failed" >&2
    fail=1
  fi
else
  echo "# PyYAML unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "code_review_workflow_test: FAILED" >&2
  exit 1
fi
echo "code_review_workflow_test: all passed"
