#!/usr/bin/env bash
# install.sh under the bash `curl | bash` gets on macOS: Apple's /bin/bash 3.2.
#
# Run as: bash tests/install_sh_bash32_test.sh
#   INSTALL_SH_BASH=/bin/bash      the bash that runs install.sh (default)
#   INSTALL_SH_REQUIRE_BASH3=1     fail unless that bash really is 3.x (ci-macos)
#
# Portable on purpose (BSD and GNU tools, no setsid / script / python): the
# full scenario suite (tests/install_sh_test.sh) needs util-linux and runs on
# the ubuntu leg. This one runs the real script with the real built
# toolu-runner from a file:// release fixture (curl reads file:// like https),
# into a temporary HOME: install, re-run, PATH in a login shell, setup options
# with and without a setup command, a tampered tarball, and --uninstall.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$ROOT/install.sh"
BASH_UNDER_TEST="${INSTALL_SH_BASH:-/bin/bash}"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[[ -x "$BASH_UNDER_TEST" ]] || fail "$BASH_UNDER_TEST not found"
bash_version="$("$BASH_UNDER_TEST" -c 'echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}"')"
echo "install.sh runs under $BASH_UNDER_TEST $bash_version"
if [[ "${INSTALL_SH_REQUIRE_BASH3:-}" == "1" && "$bash_version" != 3.* ]]; then
  fail "INSTALL_SH_REQUIRE_BASH3=1 but $BASH_UNDER_TEST is bash $bash_version"
fi

REAL_BIN="${TOOLU_RUNNER_TEST_BIN:-$ROOT/target/debug/toolu-runner}"
if [[ ! -x "$REAL_BIN" && -z "${TOOLU_RUNNER_TEST_BIN:-}" ]]; then
  (cd "$ROOT" && cargo build -q -p toolu-runner --bin toolu-runner)
fi
[[ -x "$REAL_BIN" ]] || fail "real toolu-runner binary not found at $REAL_BIN"
VERSION="$("$REAL_BIN" --version | awk '{ print $2 }')"

case "$(uname -m)" in
  x86_64|amd64) ARCH=amd64 ;;
  aarch64|arm64) ARCH=arm64 ;;
  *) fail "unsupported arch $(uname -m)" ;;
esac
case "$(uname -s)" in
  Linux) OS=linux ;;
  Darwin) OS=darwin ;;
  *) fail "unsupported OS $(uname -s)" ;;
esac
ASSET="toolu-runner-$OS-$ARCH.tar.gz"

tmp="$(mktemp -d -t install-sh-bash32.XXXXXX)"
trap 'rm -rf "$tmp"' EXIT

sha256_line() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -- "$@"; else shasum -a 256 -- "$@"; fi
}
# make_release <name> <binary>: <tmp>/srv/<name>/latest/download/{asset,SHA256SUMS}
make_release() {
  local dir="$tmp/srv/$1/latest/download"
  mkdir -p "$dir"
  bash "$ROOT/scripts/package-release.sh" "$OS" "$ARCH" "$2" "$dir" >/dev/null
  (cd "$dir" && sha256_line "$ASSET" >SHA256SUMS)
}
make_release real "$REAL_BIN"
make_release tampered "$REAL_BIN"
printf 'tampered' >>"$tmp/srv/tampered/latest/download/$ASSET"
mkdir -p "$tmp/fixture"
cat >"$tmp/fixture/toolu-runner" <<'EOF'
#!/bin/sh
case "$1" in
  --version) echo "toolu-runner 9.9.9" ;;
  setup)
    shift
    if [ "$1" = "--help" ]; then exit 0; fi
    printf '%s\n' "$@" >"$FIXTURE_LOG"
    ;;
  *) exit 2 ;;
esac
EOF
chmod 0755 "$tmp/fixture/toolu-runner"
make_release setup "$tmp/fixture/toolu-runner"

# run <home> <release> [args...] -> OUT, RC (no terminal: stdin /dev/null)
run() {
  local home="$1" rel="$2"
  shift 2
  RC=0
  OUT="$(env -i "HOME=$home" "PATH=/usr/bin:/bin:/usr/sbin:/sbin" TERM=dumb \
    "TOOLU_RUNNER_RELEASE_URL=file://$tmp/srv/$rel" "TOOLU_RUNNER_INSTALL_DIR=$home/.local/bin" \
    "FIXTURE_LOG=$home/setup.log" "$BASH_UNDER_TEST" "$SCRIPT" "$@" </dev/null 2>&1)" || RC=$?
}
blocks() { grep -c '^# >>> toolu-runner >>>' "$1" || true; }

h="$tmp/home"
mkdir -p "$h"
printf '# user bashrc, no final newline' >"$h/.bashrc"
printf '# user profile\n' >"$h/.profile"
cp "$h/.bashrc" "$tmp/bashrc.orig"
cp "$h/.profile" "$tmp/profile.orig"

echo "check: --check with and without setup options"
run "$h" real --check
[[ "$RC" -eq 0 ]] || fail "--check should exit 0 (rc=$RC): $OUT"
echo "$OUT" | grep -q 'setup args:    (none)' || fail "--check should list no setup args: $OUT"
run "$h" real --check --source github --token secret-value --yes
echo "$OUT" | grep -q 'setup args:    --source github --token •••• --yes' || fail "--check setup args: $OUT"

echo "install: verified, version from the binary, env files"
run "$h" real
[[ "$RC" -eq 0 ]] || fail "install should exit 0 (rc=$RC): $OUT"
echo "$OUT" | grep -q "^toolu-runner: installed $VERSION (checksum ok)\$" || fail "install line: $OUT"
echo "$OUT" | grep -q '^next: toolu-runner setup' || fail "next line: $OUT"
cmp -s "$REAL_BIN" "$h/.local/bin/toolu-runner" || fail "installed binary differs"
[[ -f "$h/.toolu-runner/env" ]] || fail "env file missing"

echo "re-run: still one block per profile"
run "$h" real
[[ "$RC" -eq 0 ]] || fail "re-run should exit 0 (rc=$RC): $OUT"
for f in .profile .bashrc; do
  [[ "$(blocks "$h/$f")" -eq 1 ]] || fail "$f should hold one block"
done
got="$(env -i "HOME=$h" PATH=/usr/bin:/bin TERM=dumb "$BASH_UNDER_TEST" -lc 'command -v toolu-runner' 2>/dev/null)" || true
[[ "$got" == "$h/.local/bin/toolu-runner" ]] || fail "a login shell should find the binary, got '$got'"

echo "setup options: unattended setup exits 2; a setup command gets them"
run "$h" real --source github --org acme
[[ "$RC" -eq 2 ]] || fail "setup options unattended should exit 2 (rc=$RC): $OUT"
run "$h" setup --source github --org "acme corp" -- --yes
[[ "$RC" -eq 0 ]] || fail "setup handoff should exit 0 (rc=$RC): $OUT"
[[ "$(tr '\n' '|' <"$h/setup.log")" == "--source|github|--org|acme corp|--yes|" ]] \
  || fail "setup argv: $(cat "$h/setup.log")"
run "$h" setup --no-setup
[[ "$RC" -eq 0 ]] || fail "--no-setup should exit 0 (rc=$RC): $OUT"

echo "tampered: refused, the installed binary is kept"
run "$h" tampered
[[ "$RC" -eq 1 ]] || fail "tampered should exit 1 (rc=$RC): $OUT"
echo "$OUT" | grep -q "SHA256 mismatch for $ASSET. Nothing was installed." || fail "mismatch line: $OUT"
"$h/.local/bin/toolu-runner" --version >/dev/null || fail "the previous binary should still run"

echo "uninstall: profiles byte-identical"
run "$h" real --uninstall
[[ "$RC" -eq 0 ]] || fail "--uninstall should exit 0 (rc=$RC): $OUT"
cmp -s "$tmp/bashrc.orig" "$h/.bashrc" || fail ".bashrc not byte-identical after --uninstall"
cmp -s "$tmp/profile.orig" "$h/.profile" || fail ".profile not byte-identical after --uninstall"
[[ ! -e "$h/.toolu-runner/env" ]] || fail "--uninstall should remove the env file"

echo "install_sh_bash32_test passed"
