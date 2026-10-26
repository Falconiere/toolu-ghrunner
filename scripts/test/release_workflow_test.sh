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
  local desc="$1" pat="$2" file="${3:-$WF}"
  if grep -Eq -- "$pat" "$file"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in $file: $pat" >&2
    fail=1
  fi
}
# The build and publish recipes live in scripts shared with scripts/deploy.sh.
BUILD_SH="scripts/build-release.sh"
PUBLISH_SH="scripts/publish-release.sh"

# --- trigger ---
want "triggers on tag push"            "tags: \['v\*'\]"
# --- job graph ---
want "build needs verify"              "needs: verify"
# A release scripts/deploy.sh already published skips the whole run.
want "verify needs the published check" "needs: published"
want "verify skips a published release" "if: needs\.published\.outputs\.exists != 'true'"
want "published check reads the release" 'gh release view "\$\{GITHUB_REF_NAME\}"'
want "only 'not found' means unpublished" '"[$]err" == \*"release not found"\*'
want "publish needs build"             "needs: build"
# --- gate (verify) ---
want "asserts tag == Cargo version"    "assert-version\.sh"
want "runs cargo fmt check"            "cargo fmt --all -- --check"
want "runs clippy -D warnings"         "cargo clippy --workspace --all-targets -- -D warnings"
want "runs cargo test"                 "cargo test --workspace"
# --- Rust build cache (#110): the gate job restores it after the toolchain; the
# tag-only build matrix does not (low hit rate, a cold tag build is the norm).
job_body() {
  awk -v key="  $1:" '
    $0 == key                                  { injob = 1; print; next }
    injob && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 0 }
    injob                                      { print }
  ' "$WF"
}
VERIFY_JOB=$(job_body verify)
BUILD_JOB=$(job_body build)
if printf '%s\n' "$VERIFY_JOB" | awk '
    /dtolnay\/rust-toolchain@/ { tc = 1 }
    /Swatinem\/rust-cache@v[0-9]+/ { if (tc) ok = 1 }
    END { exit !ok }'; then
  echo "ok: verify restores a Rust cache after the toolchain"
else
  echo "FAIL: verify needs a Swatinem/rust-cache step after dtolnay/rust-toolchain" >&2
  fail=1
fi
if printf '%s\n' "$BUILD_JOB" | grep -Eq -- "Swatinem/rust-cache"; then
  echo "FAIL: the release build matrix must not use rust-cache (tag-only, low hit rate)" >&2
  fail=1
else
  echo "ok: release build matrix is not cached"
fi
# --- build (4 native targets) ---
want "macos-15 (darwin arm64)"         "runner: macos-15"
want "macos-15-intel (darwin amd64)"   "runner: macos-15-intel"
want "ubuntu-24.04 (linux amd64)"      "runner: ubuntu-24\.04[[:space:]]*$"
want "ubuntu-24.04-arm (linux arm64)"  "runner: ubuntu-24\.04-arm"
want "builds via build-release.sh"     'bash scripts/build-release\.sh "\$\{\{ matrix\.os \}\}" "\$\{\{ matrix\.arch \}\}" dist'
want "builds with --locked"            "cargo build --release --locked --bin toolu-runner" "$BUILD_SH"
want "packages via package-release.sh" "package-release\.sh" "$BUILD_SH"
want "fails if no artifact"            "if-no-files-found: error"
want "toolchain pinned 1.94.1"         "dtolnay/rust-toolchain@1\.94\.1"
# --- toolu-daemon: linux-only (crates/daemon needs a local Docker socket —
# there is no macOS build to match against these linux-only assertions) ---
want "builds toolu-daemon"             "cargo build --release --locked --bin toolu-daemon" "$BUILD_SH"
want "daemon build gated to linux"     'if \[\[ "[$]os" == linux \]\]' "$BUILD_SH"
want "daemon upload gated to linux"    "if: matrix\.os == 'linux'"
want "daemon packaged via package-daemon-release.sh" "package-daemon-release\.sh" "$BUILD_SH"
want "daemon tarball uploaded as an artifact" "name: toolu-daemon-\\\$\{\{ matrix\.os \}\}-\\\$\{\{ matrix\.arch \}\}"
# --- publish ---
want "least-privilege default"         "^permissions:"
want "contents: read default"          "contents: read"
want "publish grants contents: write"  "contents: write"
want "notes from changelog-extract.sh" "changelog-extract\.sh"
want "publishes via publish-release.sh" 'bash scripts/publish-release\.sh "\$\{GITHUB_REF_NAME\}" dist release-notes\.md'
want "computes SHA256SUMS"             "sha256 -- \*\.tar\.gz >SHA256SUMS" "$PUBLISH_SH"
want "prerelease on tag with '-'"      '"[$]tag" == \*-\*' "$PUBLISH_SH"
want "passes --prerelease flag"        "args\+=\(--prerelease\)" "$PUBLISH_SH"
want "creates the release"             "gh release create" "$PUBLISH_SH"
want "release includes the daemon tarballs" '"[$]dist"/toolu-daemon-\*\.tar\.gz' "$PUBLISH_SH"
# --- downstream chain (see the workflow header: a GITHUB_TOKEN-created release
# emits no `release` event, so finalize/homebrew MUST be chained, not triggered) ---
want "one release per tag"             "^concurrency:"
want "never cancels a release"         "cancel-in-progress: false"
want "chains finalize off publish"     "uses: \./\.github/workflows/release-finalize\.yml"
want "chains homebrew off publish"     "uses: \./\.github/workflows/release-homebrew\.yml"
# --- the image workflows are chained too (#121): one gate per tag, and they
# pack the binaries `build` already produced instead of compiling again ---
want "chains the Linux image off build"  "uses: \./\.github/workflows/release-image\.yml"
want "chains the macOS image off build"  "uses: \./\.github/workflows/release-macos-image\.yml"
want "image workflows take the prebuilt binaries" "prebuilt_binaries: true"
# Linux legs build inside the image the Dockerfile's builder uses, so the
# tarball binary's glibc floor is the image runtime's (bookworm) and the
# images can ship it as-is. A ubuntu-24.04 host build links GLIBC_2.39.
want "build legs pick their container from the matrix" "container: \\\$\{\{ matrix\.container \}\}"
# Least privilege: pass the two App secrets homebrew needs, never
# `secrets: inherit` (which would forward RELEASE_PLZ_TOKEN,
# OPENROUTER_API_KEY, … as well).
want "passes the App id"               "HOMEBREW_APP_ID: \\\$\{\{ secrets\.HOMEBREW_APP_ID \}\}"
want "passes the App private key"      "HOMEBREW_APP_PRIVATE_KEY: \\\$\{\{ secrets\.HOMEBREW_APP_PRIVATE_KEY \}\}"
# Anchored to a real YAML pair, not the bare phrase: the workflow's own comment
# explains why inherit is wrong, and a looser pattern matches that prose too.
# [[:space:]] rather than \s — \s is a non-POSIX extension, and this file is the
# one asserting that others are precise.
if grep -Eq -- "^[[:space:]]+secrets:[[:space:]]+inherit[[:space:]]*$" "$WF"; then
  echo "FAIL: 'secrets: inherit' forwards every repo secret — pass HOMEBREW_APP_* explicitly" >&2
  fail=1
else
  echo "ok: no blanket secrets: inherit"
fi

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
import re, sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf.get("jobs", {})
assert set(jobs) == {"published", "verify", "build", "publish", "finalize", "homebrew", "image", "macos-image"}, f"jobs: {list(jobs)}"
assert jobs["verify"]["needs"] == "published", jobs["verify"].get("needs")
assert jobs["verify"]["if"] == "needs.published.outputs.exists != 'true'", jobs["verify"].get("if")
assert jobs["build"]["needs"] == "verify", jobs["build"].get("needs")
assert jobs["publish"]["needs"] == "build", jobs["publish"].get("needs")
inc = jobs["build"]["strategy"]["matrix"]["include"]
got = {(m["os"], m["arch"]) for m in inc}
exp = {("darwin","arm64"),("darwin","amd64"),("linux","amd64"),("linux","arm64")}
assert got == exp, f"matrix os/arch: {got}"
assert jobs["publish"]["permissions"]["contents"] == "write"
# The downstream chain: both must run AFTER the release exists. A callee is
# granted github.token automatically but sees no other secret, so homebrew is
# passed the App credentials explicitly — never `secrets: inherit`, asserted below.
for j in ("finalize", "homebrew"):
    assert jobs[j]["needs"] == "publish", f"{j} needs: {jobs[j].get('needs')}"
    assert jobs[j]["permissions"]["contents"] == "read", f"{j} perms"
assert jobs["finalize"]["uses"].endswith("release-finalize.yml"), jobs["finalize"]["uses"]
assert jobs["homebrew"]["uses"].endswith("release-homebrew.yml"), jobs["homebrew"]["uses"]
# Exactly the two App secrets cross into homebrew. `secrets: inherit` would
# forward every repo secret to a workflow that pushes to an external repo.
hb_secrets = jobs["homebrew"].get("secrets")
assert hb_secrets != "inherit", "homebrew must not inherit all repo secrets"
assert set(hb_secrets) == {"HOMEBREW_APP_ID", "HOMEBREW_APP_PRIVATE_KEY"}, (
    f"homebrew secrets: {hb_secrets}"
)
# finalize needs no secret at all: github.token is granted to callees automatically.
assert "secrets" not in jobs["finalize"], "finalize needs no secrets"
# The image workflows (#121): after `build` (its tarballs are their input),
# not after `publish` — they never read the GitHub Release. packages: write is
# granted here because a callee can only narrow the caller's token, never
# widen it. github.token is all they use, so no secrets cross.
for j, wf_file in (("image", "release-image.yml"), ("macos-image", "release-macos-image.yml")):
    assert jobs[j]["needs"] == "build", f"{j} needs: {jobs[j].get('needs')}"
    assert jobs[j]["uses"] == f"./.github/workflows/{wf_file}", jobs[j]["uses"]
    assert jobs[j].get("with") == {"prebuilt_binaries": True}, f"{j} with: {jobs[j].get('with')}"
    assert jobs[j]["permissions"] == {"contents": "read", "packages": "write"}, f"{j} perms"
    assert "secrets" not in jobs[j], f"{j} needs no secrets"
# Linux legs run in exactly the Dockerfile builder's image; darwin legs on the host.
builder = re.search(r"^FROM (\S+) AS builder$", open("Dockerfile").read(), re.M).group(1)
assert jobs["build"]["container"] == "${{ matrix.container }}", jobs["build"].get("container")
for m in inc:
    if m["os"] == "linux":
        assert m.get("container") == builder, f"linux/{m['arch']} container {m.get('container')!r} != Dockerfile builder {builder!r}"
    else:
        assert "container" not in m, f"darwin/{m['arch']} must build on the host"
print("ok: PyYAML deep-check (job DAG + 4 os/arch + publish write perm + chained finalize/homebrew + scoped App secrets + chained images + linux legs on the builder image)")
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
