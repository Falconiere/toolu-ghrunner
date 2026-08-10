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

# job_body <job-key> — everything from `<job-key>:` up to the next job key at the
# same indent (or EOF). Job-scoped extraction is what makes the assertions below
# mean anything: a whole-file grep passes as long as SOME job carries the step,
# which is exactly the drift both legs can suffer.
job_body() {
  awk -v key="  $1:" '
    $0 == key                                  { injob = 1; print; next }
    injob && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 0 }
    injob                                      { print }
  ' "$WF"
}

# want_in <job-body> <desc> <pattern> — assert a pattern is present in ONE job,
# not merely somewhere in the file. `cargo clippy …` and `cargo test --workspace`
# appear in both legs, so a whole-file grep would pass even if the macOS job had
# neither step — exactly the hole this file exists to close. The ubuntu-only
# layers (fmt, the #[allow] ban, the guardrails) need the same scoping for the
# mirror direction: moved into `ci-macos` alone they would still match the file.
want_in() {
  local body="$1" desc="$2" pat="$3"
  if printf '%s\n' "$body" | grep -Eq -- "$pat"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in the job: $pat" >&2
    fail=1
  fi
}

want_macos() { want_in "$MACOS_JOB" "$1" "$2"; }
want_ci() { want_in "$CI_JOB" "$1" "$2"; }

# --- both legs run on pull requests (a leg that never fires proves nothing) ---
want "workflow runs on pull_request"      "^  pull_request:"

# --- the macOS leg ---
want "a macOS job exists"                 "^  ci-macos:"

MACOS_JOB=$(job_body ci-macos)
CI_JOB=$(job_body ci)

if [[ -z "$MACOS_JOB" ]]; then
  echo "FAIL: could not extract the ci-macos job from $WF" >&2
  exit 1
fi

if [[ -z "$CI_JOB" ]]; then
  echo "FAIL: could not extract the ci job from $WF" >&2
  exit 1
fi

# Pinned to a macos-14+ image: macos-13 and the -intel images are x86_64, and
# arm64 is the deployment target this leg exists to cover.
want_macos "the macOS job is arm64"             "^ +runs-on: macos-(1[4-9]|[2-9][0-9])$"
want_macos "macOS runs clippy as an error gate" \
  "cargo clippy --workspace --all-targets -- -D warnings"
want_macos "macOS runs the workspace tests"     "cargo test --workspace"
# plutil only exists on macOS — validating the generated LaunchAgent as a real
# plist is the single thing this leg can do that ubuntu cannot.
want_macos "macOS validates the launchd plist"  "scripts/test/plist_test\.sh"

# --- the ubuntu leg still mirrors ./tools/check.sh all ---
want_ci "ubuntu checks formatting"         "cargo fmt --all -- --check"
want_ci "ubuntu enforces the #[allow] ban" "\./tools/check\.sh no-allow"
want_ci "ubuntu runs the guardrails"       "bash scripts/guardrails/run\.sh"
want_ci "ubuntu runs the check.sh test"    "check_script"

# The macOS leg must NOT be wired as a `continue-on-error` or `if: false`
# no-op — a green-but-inert job is worse than none, because it reads as
# coverage. Both forms are asserted: `continue-on-error` file-wide (no job may
# carry it), `if:` only inside `ci-macos`, since the other jobs are allowed
# conditions.
if grep -Eq -- "^ +continue-on-error: true" "$WF"; then
  echo "FAIL: no CI job may be continue-on-error — it reports green while failing" >&2
  fail=1
else
  echo "ok: no job is continue-on-error"
fi

if printf '%s\n' "$MACOS_JOB" | grep -Eq -- "^ +if:[[:space:]]*(false|\\\$\\{\\{[[:space:]]*false[[:space:]]*\\}\\})[[:space:]]*$"; then
  echo "FAIL: the ci-macos job is gated off with 'if: false' — it never runs" >&2
  fail=1
else
  echo "ok: the ci-macos job is not gated off"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "ci_workflow_test FAILED" >&2
  exit 1
fi

echo "ci_workflow_test passed"
