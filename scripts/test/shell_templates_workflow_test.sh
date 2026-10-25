#!/usr/bin/env bash
# shell_templates_workflow_test.sh — static validation of
# .github/workflows/shell-templates-80.yml.
#
# Three of this workflow's checks — `host (ubuntu-24.04)`, `host (macos-15)`
# and `container` — are required status contexts on main, next to ci.yml's
# `ci` / `ci-macos` and the code review. A renamed job, a dropped matrix leg or
# a retired runner image would leave the required context pending on every PR
# and block merging, and a deleted job reports nothing rather than failing, so
# the names are asserted from outside. The run-shape rules the workflow shares
# with ci.yml (superseded PR runs cancelled, main never cancelled, a timeout on
# every job, a Rust cache saved from main only) are pinned here for the same
# reason ci_workflow_test.sh pins them there.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

# SHELL_TEMPLATES_WORKFLOW_FILE lets a negative check point at a mutated copy.
WF="${SHELL_TEMPLATES_WORKFLOW_FILE:-.github/workflows/shell-templates-80.yml}"
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
    echo "FAIL: $desc — pattern not found in $WF: $pat" >&2
    fail=1
  fi
}

# --- triggers: every PR and every push to main ---
want "runs on pull_request"  "^  pull_request:"
want "runs on push"          "^  push:"

# --- the required contexts ---
want "a 'host' job exists"      "^  host:"
want "a 'container' job exists" "^  container:"
# The matrix spells the two `host (…)` contexts; macos-14 is retired on
# 2026-11-02 (actions/runner-images#13518), so the macOS leg must be 15+.
want "host runs on ubuntu-24.04 and a supported macOS image" \
  "^        os: \[ubuntu-24\.04, macos-(1[5-9]|[2-9][0-9])\]$"

# --- the run-shape rules shared with ci.yml ---
want "groups runs per ref" '^  group: shell-templates-80-\$\{\{ github\.ref \}\}$'
want "cancels superseded runs only for pull_request" \
  "^  cancel-in-progress: \\$\\{\\{ github\\.event_name == 'pull_request' \\}\\}$"
want "saves the Rust cache from main only" \
  "^          save-if: \\$\\{\\{ github\\.ref == 'refs/heads/main' \\}\\}$"

# A timeout on every job, and the toolchain before the cache in every job
# (rust-cache keys on rustc -vV), checked job by job.
job_body() {
  awk -v key="  $1:" '
    $0 == key                                  { injob = 1; print; next }
    injob && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 0 }
    injob                                      { print }
  ' "$WF"
}
for job in host container; do
  body=$(job_body "$job")
  if [[ -z "$body" ]]; then
    echo "FAIL: could not extract the $job job from $WF" >&2
    fail=1
    continue
  fi
  if printf '%s\n' "$body" | grep -Eq -- "^    timeout-minutes: [0-9]+$"; then
    echo "ok: $job has a job timeout"
  else
    echo "FAIL: $job needs timeout-minutes" >&2
    fail=1
  fi
  if printf '%s\n' "$body" | awk '
      /dtolnay\/rust-toolchain@/ { tc = 1 }
      /Swatinem\/rust-cache@v[0-9]+/ { if (tc) ok = 1 }
      END { exit !ok }'; then
    echo "ok: $job restores a Rust cache after the toolchain"
  else
    echo "FAIL: $job needs a Swatinem/rust-cache step after dtolnay/rust-toolchain" >&2
    fail=1
  fi
done

# --- no job may be inert ---
if grep -Eq -- "^    if:[[:space:]]*(false|\\\$\\{\\{[[:space:]]*false[[:space:]]*\\}\\})[[:space:]]*$" "$WF"; then
  echo "FAIL: a job is gated off with 'if: false' — it never runs" >&2
  fail=1
else
  echo "ok: no job is gated off"
fi

# --- every check above must be able to fail: replay against a mutated copy ---
# `continue-on-error: true` is legitimate on this workflow's probe STEPS (they
# assert the official runner's failure semantics), so it is not banned here,
# and the negative run instead mutates the load-bearing lines one at a time.
if [[ -z "${SHELL_TEMPLATES_WORKFLOW_FILE:-}" ]]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  mutations=(
    's/^  container:$/  containers:/'
    's/macos-15/macos-14/'
    's/^  cancel-in-progress: .*/  cancel-in-progress: false/'
    "s/^          save-if: .*/          save-if: 'true'/"
    '/^    timeout-minutes: 20$/d'
  )
  for m in "${mutations[@]}"; do
    sed -e "$m" "$WF" >"$tmp/wf.yml"
    if SHELL_TEMPLATES_WORKFLOW_FILE="$tmp/wf.yml" bash "$0" >/dev/null 2>&1; then
      echo "FAIL: the checks still pass after mutation: $m" >&2
      fail=1
    else
      echo "ok: mutation is caught: $m"
    fi
  done
fi

if [[ "$fail" -ne 0 ]]; then
  echo "shell_templates_workflow_test FAILED" >&2
  exit 1
fi

echo "shell_templates_workflow_test passed"
