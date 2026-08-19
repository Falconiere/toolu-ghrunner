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
#
# The step publishes TWO tag lines off one derivation: the lean default image
# (empty VARIANT_SUFFIX) and the Docker-capable variant (`-docker`). Both are
# run below, because the failure that matters is the docker variant landing on
# `:latest` — the tag toolu.sh's DEFAULT_RUNNER_IMAGE follows, on every
# deployment, with no deploy of its own.
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

want "derives the compat line from the minor while major is 0" 'tags\+=\(-t "\$\{image\}:v\$\{minor\}\$\{VARIANT_SUFFIX\}"\)'
want "derives the compat line from the major from 1.0 on"      'tags\+=\(-t "\$\{image\}:v\$\{major\}\$\{VARIANT_SUFFIX\}"\)'
want "keeps the stable-only guard"                             '\[\[ "\$\{GITHUB_REF_NAME\}" != \*-\* \]\]'
# The suffix is what keeps the two variants apart, so it is fed to the step by
# the matrix rather than derived inside it — assert the wiring exists, since a
# dropped `env:` block would leave the guard below firing on every publish.
want "the merge step takes its suffix from the matrix" '^          VARIANT_SUFFIX: \$\{\{ matrix\.suffix \}\}$'
want "both variants are built"                         '^        target: \[runner, docker\]$'
want "the build target is never left to the Dockerfile default" \
  '^          target: \$\{\{ matrix\.target \}\}$'
want "legs are stitched per variant, not per arch alone" \
  'pattern: digest-\$\{\{ matrix\.target \}\}-\*'

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
#
# The status is checked rather than inferred from the output. This script runs
# without `set -e`, so a failed awk would otherwise assign an empty or partial
# block and carry on into the guards below — which would report "anchors
# changed?" and send the next reader to edit a workflow that is in fact fine.
# A partial block is the nastier half: it can still satisfy both guards and
# then fail as a bash syntax error inside the harness, one indirection away
# from its cause.
if ! derivation="$(awk '
  /^        id: merge$/            { seen = 1 }
  seen && /^        run: \|$/      { inrun = 1; next }
  inrun && /^          refs=\(\)$/ { exit }
  inrun                            { sub(/^          /, ""); print }
' "$WF")"; then
  echo "FAIL: awk exited non-zero scanning $WF — could not extract the step" >&2
  exit 1
fi

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

# The unset case below is only worth anything if the step has no default of
# its own. A `VARIANT_SUFFIX="${VARIANT_SUFFIX:-}"` or `: "${VARIANT_SUFFIX=}"`
# anywhere in the block would make "unset" indistinguishable from the runner
# variant's deliberately empty value BEFORE the guard ever reads it — the
# guard would then pass on a matrix that stopped passing the suffix, and the
# unset case would be asserting the behaviour of a default rather than of the
# guard. Checked here, against the extracted text, rather than trusted.
if grep -Eq 'VARIANT_SUFFIX=|VARIANT_SUFFIX:?[-=]' <<<"$derivation"; then
  echo "FAIL: the merge step defaults or assigns VARIANT_SUFFIX itself — the unset case below" >&2
  echo "      would then exercise that default, not the fail-closed guard" >&2
  echo "  offending line(s): $(grep -E 'VARIANT_SUFFIX=|VARIANT_SUFFIX:?[-=]' <<<"$derivation" | tr '\n' ' ')" >&2
  fail=1
else
  echo "ok: the merge step never gives VARIANT_SUFFIX a default of its own"
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
  local ref="$1" suffix="$2"
  GITHUB_REF_NAME="$ref" \
  GITHUB_REPOSITORY_OWNER="Falconiere" \
  IMAGE_BASENAME="toolu-ghrunner" \
  VARIANT_SUFFIX="$suffix" \
    bash "$harness"
}

# ref + variant suffix -> the complete, ordered set of refs the step must
# publish. Asserted as full identity: a case that gained or lost a tag is a
# failure, not a subset match.
expect() {
  local ref="$1" suffix="$2" want_tags="$3" got
  got="$(run_case "$ref" "$suffix")" || {
    echo "FAIL: $ref ${suffix:-(default)} — derivation exited non-zero" >&2
    fail=1
    return
  }
  if [[ "$got" == "$want_tags" ]]; then
    echo "ok: $ref ${suffix:-(default)} -> $(tr '\n' ' ' <<<"$got")"
  else
    echo "FAIL: $ref ${suffix:-(default)}" >&2
    echo "  want: $(tr '\n' ' ' <<<"$want_tags")" >&2
    echo "  got:  $(tr '\n' ' ' <<<"$got")" >&2
    fail=1
  fi
}

IMG="ghcr.io/falconiere/toolu-ghrunner"

# --- the default image: an empty suffix, the tags every provider follows ---

# Stable: exact version + latest + the compat line.
expect v0.6.3 "" "$IMG:0.6.3
$IMG:latest
$IMG:v6"
# The minor is the compat boundary while the major is 0 — 0.7.0 breaks 0.6.x,
# so it must NOT land on :v6.
expect v0.7.0 "" "$IMG:0.7.0
$IMG:latest
$IMG:v7"
# Two-digit minor: guards against a substring/lexical derivation yielding :v1.
expect v0.10.0 "" "$IMG:0.10.0
$IMG:latest
$IMG:v10"
# From 1.0.0 the major takes over. The series restarts here by design — :v1 is
# newer than :v7 (see docs/container-image.md).
expect v1.0.0 "" "$IMG:1.0.0
$IMG:latest
$IMG:v1"
# A minor bump inside 1.x must stay on :v1 rather than minting :v1 -> :v2.
expect v1.1.0 "" "$IMG:1.1.0
$IMG:latest
$IMG:v1"
expect v2.0.0 "" "$IMG:2.0.0
$IMG:latest
$IMG:v2"
# Prereleases get the exact version ONLY. A tag consumers follow must never
# move onto an unreleased build — neither :latest nor the compat line.
expect v0.7.0-rc.1 "" "$IMG:0.7.0-rc.1"
expect v1.0.0-rc.1 "" "$IMG:1.0.0-rc.1"

# --- the Docker-capable variant: the same rules, one suffix later ---------
# Every tag it publishes must carry the suffix. The one that matters is
# `:latest-docker`: a variant that reached bare `:latest` would put a
# dockerd-carrying image in front of every toolu.sh deployment on the next
# pull, since DEFAULT_RUNNER_IMAGE follows that tag.
expect v0.6.3 -docker "$IMG:0.6.3-docker
$IMG:latest-docker
$IMG:v6-docker"
expect v1.1.0 -docker "$IMG:1.1.0-docker
$IMG:latest-docker
$IMG:v1-docker"
# The prerelease rule is about the REF, not the variant: a suffix must not
# smuggle `-rc.1` past the stable-only guard, and must not earn the variant a
# moving tag of its own either.
expect v0.7.0-rc.1 -docker "$IMG:0.7.0-rc.1-docker"

# --- an unset suffix publishes nothing at all -----------------------------
# The variants differ by this one variable, so a matrix that stopped passing
# it would silently republish whichever legs it downloaded onto `:latest`.
# The step must fail closed instead — and the runner variant's own suffix is
# deliberately EMPTY, so "unset" and "empty" have to stay distinguishable.
unset_out="$(env -u VARIANT_SUFFIX \
  GITHUB_REF_NAME=v1.2.3 \
  GITHUB_REPOSITORY_OWNER="Falconiere" \
  IMAGE_BASENAME="toolu-ghrunner" \
  bash "$harness" 2>&1)"
unset_code=$?
if [[ "$unset_code" -eq 0 ]]; then
  echo "FAIL: an unset VARIANT_SUFFIX must fail the step, not default to the lean tags" >&2
  echo "  got: $(tr '\n' ' ' <<<"$unset_out")" >&2
  fail=1
else
  echo "ok: an unset VARIANT_SUFFIX fails closed (exit $unset_code)"
fi
# A nonzero exit alone does not prove the GUARD fired: a syntax error, a
# `set -u` inherited from somewhere, or an unrelated failure earlier in the
# block would all exit nonzero and read as a pass. Demand the guard's own
# words, so this case can only go green for the one reason it is here to
# check.
if grep -q "VARIANT_SUFFIX is unset" <<<"$unset_out"; then
  echo "ok: …and it is the fail-closed guard that says so, not an incidental error"
else
  echo "FAIL: the step exited nonzero without the guard's own message — it failed for" >&2
  echo "      some other reason, so nothing here proves the guard is live" >&2
  echo "  got: $(tr '\n' ' ' <<<"$unset_out")" >&2
  fail=1
fi
if grep -q ":latest" <<<"$unset_out"; then
  echo "FAIL: an unset VARIANT_SUFFIX reached :latest — $(tr '\n' ' ' <<<"$unset_out")" >&2
  fail=1
else
  echo "ok: …and no tag is derived on the way out"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_image_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_image_workflow_test: all passed"
