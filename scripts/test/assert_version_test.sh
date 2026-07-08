#!/usr/bin/env bash
# assert_version_test.sh — real-data tests for scripts/assert-version.sh.
#
# Runs against the repo's own Cargo.toml (no mocks): derives the real
# [workspace.package] version with an independent grep (a different parser than
# the script's awk, so it's a genuine cross-check), then asserts the match,
# no-'v', mismatch, missing-arg, and bad-manifest paths.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SCRIPT="scripts/assert-version.sh"
fail=0

check() {
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

# Independent read of the real workspace version — kept bump-proof by deriving
# the matching + mismatching tags from it rather than hardcoding a number.
ver="$(grep -A6 '^\[workspace.package\]' Cargo.toml | grep -m1 '^version' | sed -E 's/.*"([^"]+)".*/\1/')"
if [[ -z "$ver" ]]; then
  echo "FAIL: test setup — could not read workspace version from Cargo.toml" >&2
  exit 1
fi
echo "# real workspace version: $ver"

check "matching tag (with v)"   0 "v$ver"
check "matching tag (no v)"     0 "$ver"
check "mismatched tag"          1 "v${ver}-mismatch"
check "missing ref arg"         2
check "nonexistent manifest"    2 "v$ver" "/no/such/Cargo.toml"

if [[ "$fail" -ne 0 ]]; then
  echo "assert_version_test: FAILED" >&2
  exit 1
fi
echo "assert_version_test: all passed"
