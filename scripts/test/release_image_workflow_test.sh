#!/usr/bin/env bash
# release_image_workflow_test.sh — static + behavioural validation of the tag
# derivation in .github/workflows/release-image.yml.
#
# A tag push can't be exercised offline, and a wrong tag here is only visible
# AFTER it is published to a public registry that consumers may already have
# pinned — retracting a moved tag is not a thing. So rather than assert on the
# shape of the script, this EXTRACTS the real `merge` step out of the workflow
# and RUNS it against a table of refs, asserting the exact tag set each one
# produces.
#
# The extraction stops at `refs=()` — everything past it needs the per-arch
# digests from a real build. That cut point is asserted below, so restructuring
# the step fails this test loudly instead of silently testing nothing.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-image.yml"
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

want "derives the compat line from the minor while major is 0" 'tags\+=\(-t "\$\{image\}:v\$\{minor\}"\)'
want "derives the compat line from the major from 1.0 on"      'tags\+=\(-t "\$\{image\}:v\$\{major\}"\)'
want "keeps the stable-only guard"                             '\[\[ "\$\{GITHUB_REF_NAME\}" != \*-\* \]\]'

# ---------------------------------------------------------------------------
# Behavioural tier: run the real derivation.
# ---------------------------------------------------------------------------

# The extraction below is anchored on two lines. Assert each exists EXACTLY
# once BEFORE extracting, rather than inferring it from the result: if the
# `id: merge` anchor is dropped, awk's `seen` never trips and it would fall
# through to whatever later step happens to carry a `run: |` block — a
# non-empty extraction of the WRONG code, which every guard on the extracted
# text (non-empty, contains a tags array) would happily accept. A duplicated
# anchor is rejected for the same reason: awk would take the first match, and
# which step that is stops being obvious.
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
assert_anchor "step-id (extraction start)" '^        id: merge$'
assert_anchor "refs=() (extraction cut)"   '^          refs=()$'

# Pull the `merge` step's run block, dedented, up to the digest-collection
# line. `sub` strips only the block's own 10-space indent, so the relative
# indentation of the if/fi bodies survives.
derivation="$(awk '
  /^        id: merge$/            { seen = 1 }
  seen && /^        run: \|$/      { inrun = 1; next }
  inrun && /^          refs=\(\)$/ { exit }
  inrun                            { sub(/^          /, ""); print }
' "$WF")"

# Belt-and-braces on top of the anchor checks: the `run: |` line between them
# could still move or change shape.
if [[ -z "$derivation" ]]; then
  echo "FAIL: could not extract the merge step's run block — anchors changed?" >&2
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
  # Emit one ref per line, dropping only the `-t` separators — the image
  # prefix is kept so a change to the registry host or the owner-lowercasing
  # surfaces as a diff here rather than being silently normalised away.
  cat <<'SH'
for t in "${tags[@]}"; do
  [[ "$t" == "-t" ]] && continue
  printf '%s\n' "$t"
done
SH
} >"$harness"

# owner deliberately mixed-case: the step lowercases it via ${VAR,,}, and a
# capital in a repository name is a push-time error at GHCR.
run_case() {
  local ref="$1"
  GITHUB_REF_NAME="$ref" \
  GITHUB_REPOSITORY_OWNER="Falconiere" \
  IMAGE_BASENAME="toolu-ghrunner" \
    bash "$harness"
}

# ref -> the complete, ordered set of refs the step must publish. Asserted as
# full identity: a case that gained or lost a tag is a failure, not a subset
# match.
expect() {
  local ref="$1" want_tags="$2" got
  got="$(run_case "$ref")" || {
    echo "FAIL: $ref — derivation exited non-zero" >&2
    fail=1
    return
  }
  if [[ "$got" == "$want_tags" ]]; then
    echo "ok: $ref -> $(tr '\n' ' ' <<<"$got")"
  else
    echo "FAIL: $ref" >&2
    echo "  want: $(tr '\n' ' ' <<<"$want_tags")" >&2
    echo "  got:  $(tr '\n' ' ' <<<"$got")" >&2
    fail=1
  fi
}

IMG="ghcr.io/falconiere/toolu-ghrunner"

# Stable: exact version + latest + the compat line.
expect v0.6.3 "$IMG:0.6.3
$IMG:latest
$IMG:v6"
# The minor is the compat boundary while the major is 0 — 0.7.0 breaks 0.6.x,
# so it must NOT land on :v6.
expect v0.7.0 "$IMG:0.7.0
$IMG:latest
$IMG:v7"
# Two-digit minor: guards against a substring/lexical derivation yielding :v1.
expect v0.10.0 "$IMG:0.10.0
$IMG:latest
$IMG:v10"
# From 1.0.0 the major takes over. The series restarts here by design — :v1 is
# newer than :v7 (see docs/container-image.md).
expect v1.0.0 "$IMG:1.0.0
$IMG:latest
$IMG:v1"
# A minor bump inside 1.x must stay on :v1 rather than minting :v1 -> :v2.
expect v1.1.0 "$IMG:1.1.0
$IMG:latest
$IMG:v1"
expect v2.0.0 "$IMG:2.0.0
$IMG:latest
$IMG:v2"
# Prereleases get the exact version ONLY. A tag consumers follow must never
# move onto an unreleased build — neither :latest nor the compat line.
expect v0.7.0-rc.1 "$IMG:0.7.0-rc.1"
expect v1.0.0-rc.1 "$IMG:1.0.0-rc.1"

if [[ "$fail" -ne 0 ]]; then
  echo "release_image_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_image_workflow_test: all passed"
