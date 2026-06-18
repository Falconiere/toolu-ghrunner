#!/usr/bin/env bash
# Smoke tests for install.sh.
# Run as: bash tests/install_sh_test.sh
set -euo pipefail

SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/install.sh"

if [[ ! -f "$SCRIPT" ]]; then
  echo "FAIL: $SCRIPT not found" >&2
  exit 1
fi

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# Test 1: --help prints usage and exits 0
echo "test 1: --help"
help_out="$("$SCRIPT" --help)"
echo "$help_out" | grep -q "install.sh" || fail "--help output missing 'install.sh'"

# Test 2: -h also works and exits 0
"$SCRIPT" -h >/dev/null || fail "-h should exit 0"

# Test 3: --check prints the install plan and exits 0
echo "test 3: --check"
output="$("$SCRIPT" --check)"
echo "$output" | grep -q 'arch:'         || fail "--check missing 'arch:'"
echo "$output" | grep -q 'os:'           || fail "--check missing 'os:'"
echo "$output" | grep -q 'version:'      || fail "--check missing 'version:'"
echo "$output" | grep -q 'install dir:'  || fail "--check missing 'install dir:'"
echo "$output" | grep -q 'would download:' || fail "--check missing 'would download:'"

# Test 4: --check includes the right URL pattern
echo "$output" | grep -q 'github.com/.*/releases/latest/download/' \
  || fail "--check should mention releases/latest/download"

# Test 5: --check with --version uses releases/download/<v>/...
out_v="$("$SCRIPT" --check --version v0.1.0)"
echo "$out_v" | grep -q '/releases/download/v0.1.0/' \
  || fail "--check --version should pin to /releases/download/<v>/"
echo "$out_v" | grep -q 'version:       v0.1.0' \
  || fail "--check --version should show pinned version"

# Test 6: --check with --install-dir echoes the override
out_d="$("$SCRIPT" --check --install-dir /opt/bin)"
echo "$out_d" | grep -q 'install dir:   /opt/bin' \
  || fail "--check --install-dir should echo the override"

# Test 7: --check with --service shows service: yes
out_s="$("$SCRIPT" --check --service)"
echo "$out_s" | grep -q 'service:       yes' \
  || fail "--check --service should show service: yes"

# Test 8: bash -n validates syntax
echo "test 8: bash -n"
bash -n "$SCRIPT" || fail "bash -n $SCRIPT failed"

# Test 9: unknown arg exits non-zero
echo "test 9: unknown arg"
if "$SCRIPT" --bogus >/dev/null 2>&1; then
  fail "--bogus should exit non-zero"
fi

# Test 10: invalid --version rejects non-semver
if "$SCRIPT" --check --version not-a-version >/dev/null 2>&1; then
  fail "--version not-a-version should be rejected"
fi
if "$SCRIPT" --check --version 0.1.0 >/dev/null 2>&1; then
  fail "--version 0.1.0 (missing 'v' prefix) should be rejected"
fi

# Test 11: --version missing value is rejected
if "$SCRIPT" --version >/dev/null 2>&1; then
  fail "--version with no value should be rejected"
fi

# Test 12: --install-dir missing value is rejected
if "$SCRIPT" --install-dir >/dev/null 2>&1; then
  fail "--install-dir with no value should be rejected"
fi

# Test 13: --service is accepted as a flag with no value
"$SCRIPT" --check --service >/dev/null || fail "--service as flag should be accepted"

echo "install.sh smoke tests passed"
