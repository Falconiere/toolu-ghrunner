#!/usr/bin/env bash
# daemon_live_workflow_test.sh — static validation of
# .github/workflows/daemon-live.yml.
#
# ci_workflow_test.sh only ever looks at ci.yml, so nothing else asserted this
# workflow's shape — an unregistered file rots the same way ci_workflow_test.sh's
# own module docs describe for the macOS leg it exists to protect. Two
# regressions this guards against specifically: the job quietly moving onto a
# GitHub-hosted runner (which has no Docker at all, let alone sysbox-runc —
# crates/daemon/tests/docker_live.rs's module docs), and the Docker
# precondition turning into a silent skip instead of a hard failure — the exact
# shape `live.yml` and `live-ghes.yml` use on purpose for an *optional* secret,
# which this workflow's Docker requirement is not.
#
# `-e` on purpose: every command here that is ALLOWED to fail is already a
# condition (`if grep …`), so anything else exiting nonzero is a bug in this
# script, and a test script that keeps going after one is a test script that
# can report a false pass. The `fail` accumulator stays — it is what lets one
# run report every missing pattern instead of only the first.
set -euo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/daemon-live.yml"
fail=0

if [[ ! -f "$WF" ]]; then
  echo "FAIL: $WF not found" >&2
  exit 1
fi

# want <desc> <pattern> — assert a pattern is PRESENT in the workflow.
want() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in $WF: $pat" >&2
    fail=1
  fi
}

# want_absent <desc> <pattern> — assert a pattern is ABSENT from the workflow.
want_absent() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "FAIL: $desc — pattern must not appear in $WF: $pat" >&2
    fail=1
  else
    echo "ok: $desc"
  fi
}

# --- the toolu-operated box, never a GitHub-hosted runner ---
want "the job runs on a self-hosted runner" "runs-on: \[self-hosted"
want "the job runs on the same label the repo's other *-live.yml workflows use" \
  "toolu-runner-v1"
want_absent "the job does not fall back to a GitHub-hosted runner" "runs-on: ubuntu-latest"
want_absent "the job does not fall back to the macOS-hosted runner" "runs-on: macos-"

# --- manual trigger only: a real Docker box is not something to dispatch on every push ---
want "the workflow can be dispatched by hand" "workflow_dispatch:"
want_absent "the workflow does not run on every push" "^  push:"
want_absent "the workflow does not run on every pull request" "^  pull_request:"

# --- hard requirements, checked explicitly rather than silently skipped ---
want "Docker reachability is checked, and a failure here fails the job" "docker info"
want "sysbox-runc's registration is checked explicitly, not assumed" "sysbox-runc"

# --- the live suite itself ---
want "the ignored live suite is run against the daemon package" \
  "cargo test -p daemon -- --ignored"
want "a failing or empty test run is treated as a failure, not a silent skip" \
  "no live test binary reported passing ignored tests"

# --- cleanup is verified, not assumed ---
want "leftover job containers are checked for" 'docker ps -a --filter label=sh\.toolu\.job-id'
want "the leftover-container check runs even when an earlier step failed" "if: always\(\)"

# --- no silently-green job ---
if grep -Eq -- "^ +continue-on-error: true" "$WF"; then
  echo "FAIL: the job must not be continue-on-error — it would report green while failing" >&2
  fail=1
else
  echo "ok: the job is not continue-on-error"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "daemon_live_workflow_test FAILED" >&2
  exit 1
fi

echo "daemon_live_workflow_test passed"
