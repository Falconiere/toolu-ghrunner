#!/usr/bin/env bash
# package_release_test.sh — real-data tests for scripts/package-release.sh.
#
# Uses the repo's real scripts/*.{plist,service} service files (no mocks). For
# the binary it prefers an actually-built toolu-runner if one exists, else it
# generates a throwaway executable file — the test asserts tar *layout* (path,
# mode, presence of both service units) against install.sh's real probe logic,
# not binary contents, so a fixture executable is a real input, not a behaviour
# mock.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SCRIPT="scripts/package-release.sh"
fail=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- pick a binary input: real build if present, else a real executable file ---
bin=""
for c in target/release/toolu-runner target/debug/toolu-runner; do
  if [[ -x "$c" ]]; then bin="$c"; break; fi
done
if [[ -z "$bin" ]]; then
  bin="$work/toolu-runner"
  printf '#!/bin/sh\necho "toolu-runner fixture"\n' >"$bin"
  chmod +x "$bin"
  echo "# using generated binary fixture (no built binary found)"
else
  echo "# using real built binary: $bin"
fi

# --- exit-code cases ---
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
expect_exit "missing args"          2
expect_exit "nonexistent binary"    2 linux amd64 /no/such/bin "$work/out"

# --- happy path: build a tarball and inspect it ---
out="$work/out"
tarball="$(bash "$SCRIPT" linux amd64 "$bin" "$out")" || {
  echo "FAIL: package-release exited non-zero on happy path" >&2
  exit 1
}

if [[ "$(basename "$tarball")" == "toolu-runner-linux-amd64.tar.gz" && -f "$tarball" ]]; then
  echo "ok: tarball named + created: $(basename "$tarball")"
else
  echo "FAIL: unexpected tarball name/path: $tarball" >&2
  fail=1
fi

# Extract and assert the layout install.sh depends on.
ex="$work/extract"
mkdir -p "$ex"
tar -xzf "$tarball" -C "$ex"

assert() {
  local desc="$1"
  shift
  if "$@"; then echo "ok: $desc"; else echo "FAIL: $desc" >&2; fail=1; fi
}
# install.sh:245 — binary probed at extract root, executable.
assert "binary at root, executable (install.sh:245)"        test -x "$ex/toolu-runner"
# install.sh install_service — scripts/ dir at root.
assert "scripts/ dir at root (install.sh --service)"        test -d "$ex/scripts"
# install.sh:287 — darwin --service plist.
assert "plist present (install.sh:287 darwin --service)"    test -f "$ex/scripts/io.toolu-runner.plist"
# install.sh:306 — linux --service unit.
assert "systemd unit present (install.sh:306 linux --service)" test -f "$ex/scripts/toolu-runner.service"

# Exact member set — no stray './' entries or extra files.
members="$(tar -tzf "$tarball" | sort | tr '\n' ' ')"
expected="scripts/ scripts/io.toolu-runner.plist scripts/toolu-runner.service toolu-runner "
if [[ "$members" == "$expected" ]]; then
  echo "ok: tarball member set is exactly the expected layout"
else
  echo "FAIL: unexpected tarball members: [$members]" >&2
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  echo "package_release_test: FAILED" >&2
  exit 1
fi
echo "package_release_test: all passed"
