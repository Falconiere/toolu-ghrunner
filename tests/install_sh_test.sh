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

# Test 7: --check with --service shows service: yes when the binary lands
# where the static unit expects it (/usr/local/bin/toolu-runner)
out_s="$("$SCRIPT" --check --service --install-dir /usr/local/bin)"
echo "$out_s" | grep -q 'service:       yes' \
  || fail "--check --service --install-dir /usr/local/bin should show service: yes"

# Test 7b: --service anywhere else is reported as skipped, naming install-service
out_s2="$("$SCRIPT" --check --service --install-dir /opt/bin)"
echo "$out_s2" | grep -q 'service:       skipped (static unit expects /usr/local/bin/toolu-runner' \
  || fail "--check --service --install-dir /opt/bin should show service: skipped"
echo "$out_s2" | grep -q 'toolu-runner install-service' \
  || fail "skipped service line should point at toolu-runner install-service"

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

# --- install dir resolution (fallback when the default is not writable) ---
# TOOLU_RUNNER_INSTALL_DIR stands in for /usr/local/bin so the test does not
# depend on the host's permissions. Root ignores mode bits, so the
# "unwritable" cases are skipped when running as root.
echo "test 14: install dir resolution"
tmp="$(mktemp -d -t install-sh-test.XXXXXX)"
trap 'chmod -R u+w "$tmp" 2>/dev/null; rm -rf "$tmp"' EXIT
fake_home="$tmp/home"
rw_dir="$tmp/rw"
ro_dir="$tmp/ro"
mkdir -p "$fake_home" "$rw_dir" "$ro_dir"
chmod 0500 "$ro_dir"

# 14a: a writable preferred dir is used as is, no note
out_rw="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$rw_dir" "$SCRIPT" --check)"
echo "$out_rw" | grep -q "install dir:   $rw_dir\$" \
  || fail "writable preferred dir should be used: $out_rw"
if echo "$out_rw" | grep -q 'note:'; then
  fail "writable preferred dir should print no fallback note"
fi

# 14b: a preferred dir that does not exist but whose parent is writable is used
out_new="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$rw_dir/sub/bin" "$SCRIPT" --check)"
echo "$out_new" | grep -q "install dir:   $rw_dir/sub/bin\$" \
  || fail "creatable preferred dir should be used: $out_new"

if [[ "$(id -u)" -ne 0 ]]; then
  # 14c: an unwritable preferred dir falls back to $HOME/.local/bin with a note
  out_ro="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$ro_dir" "$SCRIPT" --check)"
  echo "$out_ro" | grep -q "install dir:   $fake_home/.local/bin\$" \
    || fail "unwritable preferred dir should fall back to \$HOME/.local/bin: $out_ro"
  echo "$out_ro" | grep -q "note:          $ro_dir is not writable; using $fake_home/.local/bin" \
    || fail "fallback should print a note naming both dirs: $out_ro"
  echo "$out_ro" | grep -q "would install to: $fake_home/.local/bin/toolu-runner" \
    || fail "fallback should install under \$HOME/.local/bin: $out_ro"

  # 14d: a dir under an unwritable parent falls back too (mkdir -p would fail)
  out_ro2="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$ro_dir/bin" "$SCRIPT" --check)"
  echo "$out_ro2" | grep -q "install dir:   $fake_home/.local/bin\$" \
    || fail "uncreatable preferred dir should fall back: $out_ro2"

  # 14e: --service with the fallback dir is reported as skipped (the static unit
  # hardcodes /usr/local/bin/toolu-runner)
  out_ro_s="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$ro_dir" "$SCRIPT" --check --service)"
  echo "$out_ro_s" | grep -q 'service:       skipped' \
    || fail "--service with a fallback dir should be skipped: $out_ro_s"

  # 14f: an explicit --install-dir is never replaced by the fallback
  out_explicit="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$rw_dir" "$SCRIPT" --check --install-dir "$ro_dir")"
  echo "$out_explicit" | grep -q "install dir:   $ro_dir\$" \
    || fail "explicit --install-dir should be used as given: $out_explicit"
  if echo "$out_explicit" | grep -q 'note:'; then
    fail "explicit --install-dir should print no fallback note"
  fi

  # 14g: with HOME unset there is no fallback dir; the unwritable default is
  # kept (and the later writability check reports it) rather than dying on
  # an unbound variable under set -u
  out_nohome="$(env -u HOME TOOLU_RUNNER_INSTALL_DIR="$ro_dir" "$SCRIPT" --check)"
  echo "$out_nohome" | grep -q "install dir:   $ro_dir\$" \
    || fail "with HOME unset the default dir should be kept: $out_nohome"

  # 14h: a trailing slash on --install-dir is normalised, so --service still
  # matches the static unit's path
  out_slash="$("$SCRIPT" --check --service --install-dir /usr/local/bin/)"
  echo "$out_slash" | grep -q 'install dir:   /usr/local/bin$' \
    || fail "trailing slash should be stripped from --install-dir: $out_slash"
  echo "$out_slash" | grep -q 'service:       yes' \
    || fail "--install-dir /usr/local/bin/ --service should still show service: yes"

  # 14i: an explicit unwritable --install-dir fails before any download, with
  # the sudo / --install-dir hint, and nothing is written
  if err_out="$(HOME="$fake_home" "$SCRIPT" --install-dir "$ro_dir" --version v0.0.0 2>&1)"; then
    fail "explicit unwritable --install-dir should exit non-zero"
  fi
  echo "$err_out" | grep -q "no write permission to $ro_dir" \
    || fail "explicit unwritable --install-dir should name the dir: $err_out"
  echo "$err_out" | grep -q 're-run with sudo, or pass --install-dir' \
    || fail "explicit unwritable --install-dir should print the hint: $err_out"
  if echo "$err_out" | grep -q 'download:'; then
    fail "the writability check must run before the download: $err_out"
  fi
fi

echo "install.sh smoke tests passed"
