#!/usr/bin/env bash
# ci_workflow_test.sh — static validation of .github/workflows/ci.yml.
#
# The gate is ONE command locally (./tools/check.sh all) and CI is required to
# mirror it step-for-step. This pins the parts of that mirror a reader cannot
# verify by running anything: that the ubuntu leg still runs every layer, and
# that the macOS leg exists at all.
#
# The macOS leg is the regression this file exists for. `runner_os()` shipped
# hardcoded to "Linux", the launchd unit shipped without a PATH, and the Docker
# client shipped pinned to /var/run/docker.sock — three macOS-only defects that
# lived in main because nothing ever compiled, let alone tested, on a Mac.
# Deleting the leg would make that blind spot silently return, and a deleted job
# reports nothing rather than failing, so assert it from outside.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/ci.yml"
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

# --- both legs run on pull requests (a leg that never fires proves nothing) ---
want "workflow runs on pull_request"      "^  pull_request:"

# --- the macOS leg ---
# Pinned to a macos-14+ image: macos-13 and the -intel images are x86_64, and
# arm64 is the deployment target this leg exists to cover.
want "a macOS job exists"                 "^  ci-macos:"
want "the macOS job is arm64"             "^ +runs-on: macos-(1[4-9]|[2-9][0-9])$"
want "macOS runs clippy as an error gate" \
  "cargo clippy --workspace --all-targets -- -D warnings"
want "macOS runs the workspace tests"     "cargo test --workspace"
# plutil only exists on macOS — validating the generated LaunchAgent as a real
# plist is the single thing this leg can do that ubuntu cannot.
want "macOS validates the launchd plist"  "scripts/test/plist_test\.sh"

# --- the ubuntu leg still mirrors ./tools/check.sh all ---
want "ubuntu checks formatting"           "cargo fmt --all -- --check"
want "ubuntu enforces the #\[allow\] ban" "\./tools/check\.sh no-allow"
want "ubuntu runs the guardrails"         "bash scripts/guardrails/run\.sh"

# The macOS leg must NOT be wired as a `continue-on-error` or `if: false`
# no-op — a green-but-inert job is worse than none, because it reads as
# coverage.
if grep -Eq -- "^ +continue-on-error: true" "$WF"; then
  echo "FAIL: no CI job may be continue-on-error — it reports green while failing" >&2
  fail=1
else
  echo "ok: no job is continue-on-error"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "ci_workflow_test FAILED" >&2
  exit 1
fi

echo "ci_workflow_test passed"
