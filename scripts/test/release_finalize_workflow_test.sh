#!/usr/bin/env bash
# release_finalize_workflow_test.sh — static validation of
# .github/workflows/release-finalize.yml.
#
# A published-release event can't be exercised offline, so this asserts the
# invariants the finalize contract depends on: trigger, read-only permissions,
# the 4-target matrix, checksum verification, size sanity, and tarball layout.
# Uses grep (dependency-free, runs on every runner); if PyYAML is importable
# it additionally asserts the file parses and the matrix is exactly 4 targets.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-finalize.yml"
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

# --- trigger ---
# Chained from release.yml, NOT `on: release: [published]` — a release created
# by a workflow step using the default GITHUB_TOKEN emits no `release` event,
# so an event-triggered version of this workflow could never fire.
want "callable as a reusable workflow"  "^  workflow_call:"
want "reads the tag from the caller"    "TAG: \\\$\{\{ github\.ref_name \}\}"
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
# Under workflow_call there is no `release` event payload: any `github.event.release.*`
# read silently evaluates to "" and `gh release download ""` fails deep in the job.
# Matches any non-comment line, not just `${{ }}` — `if:` accepts a bare
# expression without the braces, which an expression-only pattern would miss.
reject "no release-event payload reads" '^[^#]*github\.event\.release'
reject "caller owns concurrency"        "^concurrency:"
# --- permissions (read-only: never edits the release or the repo) ---
want "least-privilege permissions"     "^permissions:"
want "contents: read only"             "contents: read"
# --- matrix (4 native targets, same set as release.yml) ---
want "darwin arm64"                    "os: darwin"
want "amd64"                           "arch: amd64"
want "linux"                           "os: linux"
want "arm64"                           "arch: arm64"
# --- smoke test steps ---
want "downloads the published tarball" "gh release download"
want "downloads SHA256SUMS"            "SHA256SUMS"
want "verifies checksum"               "sha256sum --ignore-missing -c SHA256SUMS"
want "verifies size sanity"            "size.*1048576"
want "verifies binary at tarball root" "toolu-runner scripts/io\.toolu-runner\.plist scripts/toolu-runner\.service"

# --- exactly 4 matrix targets ---
n_targets="$(grep -Ec -- "^ +- os:" "$WF")"
if [[ "$n_targets" == "4" ]]; then
  echo "ok: exactly 4 matrix targets"
else
  echo "FAIL: expected 4 matrix targets, found $n_targets" >&2
  fail=1
fi

# --- optional: deep parse if PyYAML is available ---
if python3 -c 'import yaml' >/dev/null 2>&1; then
  if python3 - "$WF" <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
# YAML 1.1 parses the bare `on:` key as the boolean True, not the string "on".
triggers = wf[True]
assert set(triggers) == {"workflow_call"}, f"triggers: {list(triggers)}"
jobs = wf.get("jobs", {})
assert set(jobs) == {"smoke-test"}, f"jobs: {list(jobs)}"
inc = jobs["smoke-test"]["strategy"]["matrix"]["include"]
got = {(m["os"], m["arch"]) for m in inc}
exp = {("darwin","arm64"),("darwin","amd64"),("linux","amd64"),("linux","arm64")}
assert got == exp, f"matrix os/arch: {got}"
assert wf["permissions"]["contents"] == "read"
print("ok: PyYAML deep-check (job set + 4 os/arch + read-only perm)")
PY
  then :; else
    echo "FAIL: PyYAML deep-check failed" >&2
    fail=1
  fi
else
  echo "# PyYAML unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_finalize_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_finalize_workflow_test: all passed"
