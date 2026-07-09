#!/usr/bin/env bash
# generate_homebrew_formula_test.sh — real-data tests for
# scripts/generate-homebrew-formula.sh against a fixture SHA256SUMS.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SCRIPT="scripts/generate-homebrew-formula.sh"
fail=0
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

sums="$tmp/SHA256SUMS"
cat >"$sums" <<'EOF'
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  toolu-runner-darwin-arm64.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  toolu-runner-darwin-amd64.tar.gz
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  toolu-runner-linux-amd64.tar.gz
dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  toolu-runner-linux-arm64.tar.gz
EOF

check() {
  local desc="$1" pat="$2" out="$3"
  if grep -Fq -- "$pat" <<<"$out"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — not found: $pat" >&2
    fail=1
  fi
}

# --- happy path ---
out="$(bash "$SCRIPT" v0.1.0 "$sums")" || { echo "FAIL: script exited non-zero on valid input" >&2; fail=1; }

check "formula class name"     'class TooluRunner < Formula'      "$out"
check "version (no v prefix)"  'version "0.1.0"'                  "$out"
check "MIT license"            'license "MIT"'                    "$out"
check "darwin arm64 url"       "toolu-runner-darwin-arm64.tar.gz" "$out"
check "darwin arm64 sha"       'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$out"
check "darwin amd64 url"       "toolu-runner-darwin-amd64.tar.gz" "$out"
check "linux amd64 url"        "toolu-runner-linux-amd64.tar.gz"  "$out"
check "linux arm64 url"        "toolu-runner-linux-arm64.tar.gz"  "$out"
check "installs the binary"    'bin.install "toolu-runner"'       "$out"
check "has a test block"       'test do'                          "$out"

if command -v ruby >/dev/null 2>&1; then
  if printf '%s' "$out" | ruby -c >/dev/null 2>&1; then
    echo "ok: generated formula is valid Ruby"
  else
    echo "FAIL: generated formula has a Ruby syntax error" >&2
    fail=1
  fi
else
  echo "# ruby unavailable — skipped syntax check"
fi

# --- error path: missing tag arg ---
if bash "$SCRIPT" >/dev/null 2>&1; then
  echo "FAIL: expected usage error with no args" >&2
  fail=1
else
  echo "ok: fails with no args"
fi

# --- error path: tag missing 'v' prefix ---
if bash "$SCRIPT" "0.1.0" "$sums" >/dev/null 2>&1; then
  echo "FAIL: expected error for tag without leading 'v'" >&2
  fail=1
else
  echo "ok: fails on tag without leading 'v'"
fi

# --- error path: missing checksum for a target ---
partial="$tmp/SHA256SUMS.partial"
grep -v darwin-arm64 "$sums" >"$partial"
if bash "$SCRIPT" v0.1.0 "$partial" >/dev/null 2>&1; then
  echo "FAIL: expected error when a target's checksum is missing" >&2
  fail=1
else
  echo "ok: fails when a target's checksum is missing"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "generate_homebrew_formula_test: FAILED" >&2
  exit 1
fi
echo "generate_homebrew_formula_test: all passed"
