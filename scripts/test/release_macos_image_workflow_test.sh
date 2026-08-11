#!/usr/bin/env bash
# release_macos_image_workflow_test.sh — static + behavioural validation of
# .github/workflows/release-macos-image.yml.
#
# Same reasoning as release_image_workflow_test.sh: a tag push cannot be
# exercised offline, and a wrong tag is only visible AFTER it is published to
# a registry consumers may already have pinned. So this EXTRACTS the real
# `Derive tags` step and RUNS it against a table of refs.
#
# The static tier additionally pins the two structural facts that make this
# workflow correct and that nothing else can catch: the binary is compiled on
# a macOS host (Rust cannot cross-compile to darwin here) and the image is
# packed on Linux (GitHub's macOS runners have no Docker daemon). Collapse the
# two jobs into one and the workflow breaks on whichever host it lands.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-macos-image.yml"
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

# job_body <job-key> — everything from `<job-key>:` up to the next job key at
# the same indent. Job-scoped, because "some job runs on macos-14" is exactly
# the assertion that stays true after the wrong job is moved.
job_body() {
  awk -v key="  $1:" '
    $0 == key                                  { injob = 1; print; next }
    injob && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 0 }
    injob                                      { print }
  ' "$WF"
}

want_in() {
  local body="$1" desc="$2" pat="$3"
  if printf '%s\n' "$body" | grep -Eq -- "$pat"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in the job: $pat" >&2
    fail=1
  fi
}

BUILD_JOB="$(job_body build)"
IMAGE_JOB="$(job_body image)"
if [[ -z "$BUILD_JOB" || -z "$IMAGE_JOB" ]]; then
  echo "FAIL: could not extract the build/image jobs from $WF — restructured?" >&2
  exit 1
fi

# --- the split that the whole workflow rests on ----------------------------
want_in "$BUILD_JOB" "the binary is compiled on a real Mac"  "runs-on: macos-14"
want_in "$BUILD_JOB" "the compile is --locked"               "cargo build --release --locked --bin toolu-runner"
want_in "$BUILD_JOB" "the artifact keeps its executable bit" "chmod \+x dist/toolu-runner-darwin-arm64"
want_in "$IMAGE_JOB" "the image is packed on Linux"          "runs-on: ubuntu-24.04"
want_in "$IMAGE_JOB" "the pack uses Dockerfile.macos"        "file: Dockerfile.macos"
want_in "$IMAGE_JOB" "the pack consumes the macOS artifact"  "name: toolu-runner-darwin-arm64"
# Pushing on a non-tag would publish an untested payload under a tag someone
# may pin; the gate is the expression, not the reviewer.
want_in "$IMAGE_JOB" "pushes on tags only"                   "push: \\\$\{\{ github.ref_type == 'tag' \}\}"

# The image these tags name must not collide with the Linux one.
want "the image basename is the macOS one" "IMAGE_BASENAME: toolu-ghrunner-macos"

# ---------------------------------------------------------------------------
# Behavioural tier: run the real derivation.
# ---------------------------------------------------------------------------

# Anchored extraction, asserted exactly once each first: a dropped anchor would
# otherwise let awk fall through to some later `run: |` block and "pass" while
# testing the wrong code.
assert_anchor() {
  local desc="$1" pat="$2" n
  n="$(grep -c -- "$pat" "$WF")"
  if [[ "$n" -ne 1 ]]; then
    echo "FAIL: expected exactly 1 $desc anchor in $WF, found $n — workflow restructured?" >&2
    echo "      the behavioural tier below cannot be trusted until this is re-anchored" >&2
    exit 1
  fi
  echo "ok: $desc anchor present exactly once"
}
assert_anchor "step-id (extraction start)" '^        id: tags$'
assert_anchor "list=() (extraction cut)"   '^          list=()$'

if ! derivation="$(awk '
  /^        id: tags$/             { seen = 1 }
  seen && /^        run: \|$/      { inrun = 1; next }
  inrun && /^          list=\(\)$/ { exit }
  inrun                            { sub(/^          /, ""); print }
' "$WF")"; then
  echo "FAIL: awk exited non-zero scanning $WF — could not extract the step" >&2
  exit 1
fi

if [[ -z "$derivation" ]]; then
  echo "FAIL: could not extract the derive-tags run block — anchors changed?" >&2
  exit 1
fi
if ! grep -q 'tags=(-t' <<<"$derivation"; then
  echo "FAIL: extracted block does not build a tags array — anchors changed?" >&2
  exit 1
fi

harness="$(mktemp)"
trap 'rm -f "$harness"' EXIT
{
  printf '%s\n' "$derivation"
  # Emit one ref per line, dropping the `-t` separators only — the registry
  # host and the owner-lowercasing stay in the output so a change to either
  # shows up here as a diff.
  cat <<'SH'
for t in "${tags[@]}"; do
  [[ "$t" == "-t" ]] && continue
  printf '%s\n' "$t"
done
SH
} >"$harness"

# owner deliberately mixed-case: the step lowercases it via ${VAR,,}, and a
# capital is a push-time error at GHCR.
run_case() {
  local ref="$1" ref_type="$2"
  GITHUB_REF_NAME="$ref" \
  GITHUB_REF_TYPE="$ref_type" \
  GITHUB_REPOSITORY_OWNER="Falconiere" \
  IMAGE_BASENAME="toolu-ghrunner-macos" \
    bash "$harness"
}

expect() {
  local ref="$1" ref_type="$2" want_tags="$3" got
  got="$(run_case "$ref" "$ref_type")" || {
    echo "FAIL: $ref — derivation exited non-zero" >&2
    fail=1
    return
  }
  if [[ "$got" == "$want_tags" ]]; then
    echo "ok: $ref ($ref_type) -> $(tr '\n' ' ' <<<"$got")"
  else
    echo "FAIL: $ref ($ref_type)" >&2
    echo "  want: $(tr '\n' ' ' <<<"$want_tags")" >&2
    echo "  got:  $(tr '\n' ' ' <<<"$got")" >&2
    fail=1
  fi
}

IMG="ghcr.io/falconiere/toolu-ghrunner-macos"

# Stable: exact version + latest + the compat line. The rule is release-image's
# rule — the two images ride the same tag and must stay legible together.
expect v0.7.4 tag "$IMG:0.7.4
$IMG:latest
$IMG:v7"
expect v0.10.0 tag "$IMG:0.10.0
$IMG:latest
$IMG:v10"
# From 1.0.0 the major takes over; the series restarts by design (:v1 is newer
# than :v7 — see docs/container-image.md).
expect v1.0.0 tag "$IMG:1.0.0
$IMG:latest
$IMG:v1"
expect v1.1.0 tag "$IMG:1.1.0
$IMG:latest
$IMG:v1"
# Prereleases get the exact version ONLY — a followed tag must never move onto
# an unreleased build.
expect v0.8.0-rc.1 tag "$IMG:0.8.0-rc.1"
# Non-tag builds (PR, workflow_dispatch on a branch) must produce a local tag
# and nothing that looks like a publishable ref. `github.ref_name` there is a
# branch or `NN/merge`, which the tag rule would happily mangle into
# `ghcr.io/...:main`.
expect main branch "toolu-ghrunner-macos:pr"
expect 42/merge pull_request "toolu-ghrunner-macos:pr"

if [[ "$fail" -ne 0 ]]; then
  echo "release_macos_image_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_macos_image_workflow_test: all passed"
