#!/usr/bin/env bash
# release_pr_script_test.sh — end-to-end tests for scripts/release-pr.sh and
# scripts/release-tag.sh in a real throwaway git repo (no mocks).
#
# Builds a fixture: minimal workspace Cargo.toml at v0.1.0 (plus a decoy
# `version = ` line in a later section that the section-scoped awk and the
# whole-line sed must both leave alone), the repo's REAL cliff.toml, a
# CHANGELOG.md seeded with the repo's real header + a pre-existing history
# section, an annotated v0.1.0 tag, then a feat: and a chore: commit. Runs
# both scripts with DRY_RUN=1 (stops before push/gh — no network) and
# asserts the bump, the changelog shape, the release commit, the tag, and
# the exit-0 guards ("no releasable commits" / "release-tag owns this push"
# / "already tagged").
#
# Requires git-cliff on PATH. ci.yml's test step does NOT install git-cliff,
# so this self-skips there (the release-pr.yml workflow pins its own copy via
# taiki-e/install-action); locally, with git-cliff installed, it runs for real.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root
repo_root="$(pwd)"

PR_SH="$repo_root/scripts/release-pr.sh"
TAG_SH="$repo_root/scripts/release-tag.sh"
fail=0

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "# git-cliff unavailable — skipped (release-pr.yml installs its own pinned copy)"
  exit 0
fi

ok() { echo "ok: $1"; }
bad() {
  echo "FAIL: $1" >&2
  fail=1
}

for s in "$PR_SH" "$TAG_SH"; do
  if [[ -x "$s" ]]; then
    ok "$(basename "$s") exists and is executable"
  else
    bad "$(basename "$s") missing or not executable"
  fi
done

# The awk reading [workspace.package] version must reset its flag on the next
# section header — the scoping fix this pattern pins (a fixture where both
# lines exist can't distinguish it functionally: the in-section match wins
# first either way, so pin the pattern itself).
scoped_awk='/^\[workspace.package\]/{f=1; next} /^\[/{f=0} f && /^version = /'
for s in "$PR_SH" "$TAG_SH"; do
  if grep -Fq "$scoped_awk" "$s"; then
    ok "$(basename "$s") scopes the awk to [workspace.package]"
  else
    bad "$(basename "$s") missing the section-scoped awk"
  fi
done

# --- fixture: a real git repo sitting on a tagged v0.1.0 release ---
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

git -C "$tmp" init -q -b main
git -C "$tmp" config user.name "fixture"
git -C "$tmp" config user.email "fixture@example.invalid"
git -C "$tmp" config commit.gpgsign false
git -C "$tmp" config tag.gpgsign false

# Decoy: a later-section version line. The trailing comment keeps it a valid
# `^version = ` prefix for the awk while staying outside the sed's whole-line
# `^version = ".*"$` match (the real root manifest has exactly one such line).
cat > "$tmp/Cargo.toml" <<'EOF'
[workspace]
members = []

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.metadata.decoy]
version = "9.9.9" # decoy — scripts must never touch this line
EOF

cp "$repo_root/cliff.toml" "$tmp/cliff.toml"
# Seed the changelog with the repo's REAL header, byte-for-byte — git-cliff's
# --prepend strips the configured [changelog] header from the old content —
# plus a history section that must survive the prepend untouched.
awk '{print} /^## \[Unreleased\]$/{print ""; exit}' "$repo_root/CHANGELOG.md" > "$tmp/CHANGELOG.md"
cat >> "$tmp/CHANGELOG.md" <<'EOF'
## [0.1.0] - 2026-01-01

### Added

- first fixture release.
EOF

git -C "$tmp" add -A
git -C "$tmp" commit -qm "chore: fixture scaffold"
git -C "$tmp" tag -a v0.1.0 -m "fixture 0.1.0"

# --- guard: nothing new since v0.1.0 -> exit 0, no PR work ---
out="$( (cd "$tmp" && DRY_RUN=1 bash "$PR_SH") 2>&1 )"
rc=$?
if [[ $rc -eq 0 && "$out" == *"no releasable commits"* ]]; then
  ok "no-new-commits guard exits 0 with 'no releasable commits'"
else
  bad "no-new-commits guard — rc=$rc, out: $out"
fi

# --- the release cycle: one releasable feat, one skipped chore ---
git -C "$tmp" commit -q --allow-empty -m "feat: add shiny fixture feature"
git -C "$tmp" commit -q --allow-empty -m "chore: noisy release plumbing"

out="$( (cd "$tmp" && DRY_RUN=1 bash "$PR_SH") 2>&1 )"
rc=$?
if [[ $rc -eq 0 ]]; then
  ok "release-pr.sh dry run exits 0"
else
  bad "release-pr.sh dry run — rc=$rc, out: $out"
fi

# feat -> minor: 0.1.0 becomes 0.2.0 in [workspace.package] only.
if grep -q '^version = "0.2.0"$' "$tmp/Cargo.toml"; then
  ok "Cargo.toml bumped to 0.2.0 (feat -> minor)"
else
  bad "Cargo.toml not bumped to 0.2.0: $(grep '^version' "$tmp/Cargo.toml")"
fi
if grep -Fq 'version = "9.9.9" # decoy — scripts must never touch this line' "$tmp/Cargo.toml"; then
  ok "decoy version line untouched"
else
  bad "decoy version line was modified"
fi
if [[ ! -f "$tmp/Cargo.toml.bak" ]]; then
  ok "no sed backup file left behind"
else
  bad "sed left Cargo.toml.bak behind"
fi

# CHANGELOG: new section prepended under the preserved header, feat listed,
# chore skipped, existing history intact and below the new section.
if grep -Eq '^## \[0\.2\.0\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$' "$tmp/CHANGELOG.md"; then
  ok "CHANGELOG gained the dated [0.2.0] section"
else
  bad "CHANGELOG missing '## [0.2.0] - <date>'"
fi
if grep -q 'add shiny fixture feature' "$tmp/CHANGELOG.md"; then
  ok "feat commit listed in the changelog"
else
  bad "feat commit missing from the changelog"
fi
if grep -q 'noisy release plumbing' "$tmp/CHANGELOG.md"; then
  bad "chore commit leaked into the changelog"
else
  ok "chore commit absent from the changelog"
fi
if [[ "$(head -1 "$tmp/CHANGELOG.md")" == "# Changelog" ]] &&
  grep -q '^## \[Unreleased\]$' "$tmp/CHANGELOG.md"; then
  ok "changelog header preserved"
else
  bad "changelog header damaged by the prepend"
fi
new_at="$(grep -n '^## \[0\.2\.0\]' "$tmp/CHANGELOG.md" | head -1 | cut -d: -f1)"
old_at="$(grep -n '^## \[0\.1\.0\]' "$tmp/CHANGELOG.md" | head -1 | cut -d: -f1)"
if [[ -n "$new_at" && -n "$old_at" && "$new_at" -lt "$old_at" ]] &&
  grep -q 'first fixture release\.' "$tmp/CHANGELOG.md"; then
  ok "history section preserved below the new section"
else
  bad "history section lost or reordered (0.2.0@${new_at:-?}, 0.1.0@${old_at:-?})"
fi

# The dry run stops after the local commit on branch release-pr.
if [[ "$(git -C "$tmp" log -1 --format=%s release-pr 2>/dev/null)" == "chore(release): v0.2.0" ]]; then
  ok "commit 'chore(release): v0.2.0' on branch release-pr"
else
  bad "release commit missing on branch release-pr"
fi
if [[ -z "$(git -C "$tmp" status --porcelain)" ]]; then
  ok "working tree clean after the release commit"
else
  bad "release commit left uncommitted changes: $(git -C "$tmp" status --porcelain)"
fi

# --- guard: untagged bump on the branch -> release-tag owns this push ---
out="$( (cd "$tmp" && DRY_RUN=1 bash "$PR_SH") 2>&1 )"
rc=$?
if [[ $rc -eq 0 && "$out" == *"release-tag owns this push"* ]]; then
  ok "untagged-bump guard exits 0 with 'release-tag owns this push'"
else
  bad "untagged-bump guard — rc=$rc, out: $out"
fi

# --- release-tag.sh: annotated local tag, push skipped ---
out="$( (cd "$tmp" && DRY_RUN=1 bash "$TAG_SH") 2>&1 )"
rc=$?
if [[ $rc -eq 0 ]]; then
  ok "release-tag.sh dry run exits 0"
else
  bad "release-tag.sh dry run — rc=$rc, out: $out"
fi
if [[ "$(git -C "$tmp" cat-file -t v0.2.0 2>/dev/null)" == "tag" ]]; then
  ok "annotated tag v0.2.0 created"
else
  bad "annotated tag v0.2.0 missing"
fi

# --- guard: version already tagged -> exit 0, no re-tag ---
out="$( (cd "$tmp" && DRY_RUN=1 bash "$TAG_SH") 2>&1 )"
rc=$?
if [[ $rc -eq 0 && "$out" == *"already tagged"* ]]; then
  ok "already-tagged guard exits 0"
else
  bad "already-tagged guard — rc=$rc, out: $out"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_pr_script_test: FAILED" >&2
  exit 1
fi
echo "release_pr_script_test: all passed"
