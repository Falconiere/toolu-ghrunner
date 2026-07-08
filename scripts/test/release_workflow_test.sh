#!/usr/bin/env bash
# release_workflow_test.sh — static validation of .github/workflows/release.yml.
#
# A real tag push can't be exercised offline, so this asserts the invariants the
# release contract depends on: trigger, job graph, the 4-target matrix, the
# gate, --locked builds, packaging + checksums, prerelease handling, and least-
# privilege permissions. Uses grep (dependency-free, runs on every runner); if
# PyYAML is importable it additionally asserts the file parses and the job DAG.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release.yml"
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
    echo "FAIL: $desc — pattern not found: $pat" >&2
    fail=1
  fi
}

# --- trigger ---
want "triggers on tag push"            "tags: \['v\*'\]"
# --- job graph ---
want "build needs verify"              "needs: verify"
want "publish needs build"             "needs: build"
# --- gate (verify) ---
want "asserts tag == Cargo version"    "assert-version\.sh"
want "runs cargo fmt check"            "cargo fmt --all -- --check"
want "runs clippy -D warnings"         "cargo clippy --workspace --all-targets -- -D warnings"
want "runs cargo test"                 "cargo test --workspace"
# --- build (4 native targets) ---
want "macos-14 (darwin arm64)"         "runner: macos-14"
want "macos-15-intel (darwin amd64)"   "runner: macos-15-intel"
want "ubuntu-24.04 (linux amd64)"      "runner: ubuntu-24\.04[[:space:]]*$"
want "ubuntu-24.04-arm (linux arm64)"  "runner: ubuntu-24\.04-arm"
want "builds with --locked"            "cargo build --release --locked"
want "packages via package-release.sh" "package-release\.sh"
want "fails if no artifact"            "if-no-files-found: error"
want "toolchain pinned 1.94.1"         "dtolnay/rust-toolchain@1\.94\.1"
# --- publish ---
want "least-privilege default"         "^permissions:"
want "contents: read default"          "contents: read"
want "publish grants contents: write"  "contents: write"
want "computes SHA256SUMS"             "sha256sum -- \*\.tar\.gz > SHA256SUMS"
want "notes from changelog-extract.sh" "changelog-extract\.sh"
want "prerelease on tag with '-'"      'GITHUB_REF_NAME.*\*-\*'
want "passes --prerelease flag"        "flag=--prerelease"
want "creates the release"             "gh release create"

# --- exactly 4 matrix targets ---
n_runners="$(grep -Ec -- "^ +- runner:" "$WF")"
if [[ "$n_runners" == "4" ]]; then
  echo "ok: exactly 4 matrix targets"
else
  echo "FAIL: expected 4 matrix targets, found $n_runners" >&2
  fail=1
fi

# --- optional: deep parse if PyYAML is available ---
if python3 -c 'import yaml' >/dev/null 2>&1; then
  if python3 - "$WF" <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf.get("jobs", {})
assert set(jobs) == {"verify", "build", "publish"}, f"jobs: {list(jobs)}"
assert jobs["build"]["needs"] == "verify", jobs["build"].get("needs")
assert jobs["publish"]["needs"] == "build", jobs["publish"].get("needs")
inc = jobs["build"]["strategy"]["matrix"]["include"]
got = {(m["os"], m["arch"]) for m in inc}
exp = {("darwin","arm64"),("darwin","amd64"),("linux","amd64"),("linux","arm64")}
assert got == exp, f"matrix os/arch: {got}"
assert jobs["publish"]["permissions"]["contents"] == "write"
print("ok: PyYAML deep-check (job DAG + 4 os/arch + publish write perm)")
PY
  then :; else
    echo "FAIL: PyYAML deep-check failed" >&2
    fail=1
  fi
else
  echo "# PyYAML unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_workflow_test: all passed"
