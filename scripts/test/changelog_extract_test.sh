#!/usr/bin/env bash
# changelog_extract_test.sh — real-data tests for scripts/changelog-extract.sh
# against the repo's own CHANGELOG.md (no mocks). Asserts the [0.1.0] section
# extracts non-empty, excludes its own heading, tolerates a leading 'v', and
# that an absent version / missing arg fail with the right exit codes.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SCRIPT="scripts/changelog-extract.sh"
fail=0

expect_exit() {
  local desc="$1" want="$2"
  shift 2
  bash "$SCRIPT" "$@" >/dev/null 2>&1
  local got=$?
  if [[ "$got" != "$want" ]]; then
    echo "FAIL: $desc — want exit $want, got $got" >&2
    fail=1
  else
    echo "ok: $desc (exit $got)"
  fi
}

# --- exit-code cases (real CHANGELOG.md) ---
expect_exit "extract present 0.1.0"        0 "0.1.0"
expect_exit "extract present with 'v'"     0 "v0.1.0"
expect_exit "absent version fails"         1 "9.9.9"
expect_exit "missing version arg"          2
expect_exit "missing changelog file"       2 "0.1.0" "/no/such/CHANGELOG.md"

# --- content assertions on the real [0.1.0] body ---
body="$(bash "$SCRIPT" 0.1.0 CHANGELOG.md)"
if [[ -z "$body" ]]; then
  echo "FAIL: extracted [0.1.0] body is empty" >&2
  fail=1
else
  echo "ok: extracted [0.1.0] body is non-empty ($(printf '%s' "$body" | wc -l | tr -d ' ') lines)"
fi
if printf '%s' "$body" | grep -q '## \[0.1.0\]'; then
  echo "FAIL: body leaked its own heading '## [0.1.0]'" >&2
  fail=1
else
  echo "ok: body excludes its own heading"
fi
# The real [0.1.0] section opens with this sentence — proves we grabbed the
# right block, not an adjacent one.
if printf '%s' "$body" | grep -q 'first release of toolu-runner'; then
  echo "ok: body contains the expected [0.1.0] content"
else
  echo "FAIL: body missing expected [0.1.0] content" >&2
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  echo "changelog_extract_test: FAILED" >&2
  exit 1
fi
echo "changelog_extract_test: all passed"
