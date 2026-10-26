#!/usr/bin/env bash
# deploy_test.sh — scripts/deploy.sh --dry-run: which refs each env pushes,
# what prod alone touches, and the argument guards. Nothing is built or pushed.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SCRIPT="scripts/deploy.sh"
fail=0
version="$(awk -F'"' '/^\[workspace.package\]/ {p=1} p && /^version/ {print $2; exit}' Cargo.toml)"

has() { # <label> <output> <fixed string>
  if grep -qF -- "$3" <<<"$2"; then echo "ok: $1"; else echo "FAIL: $1 (missing '$3')" >&2; fail=1; fi
}
lacks() { # <label> <output> <fixed string>
  if grep -qF -- "$3" <<<"$2"; then echo "FAIL: $1 (found '$3')" >&2; fail=1; else echo "ok: $1"; fi
}
exits() { # <label> <want> <args...>
  local label="$1" want="$2"
  shift 2
  bash "$SCRIPT" "$@" --dry-run >/dev/null 2>&1
  local got=$?
  if [[ $got -eq $want ]]; then echo "ok: $label"; else echo "FAIL: $label (exit $got, want $want)" >&2; fail=1; fi
}

# --- dev: -dev images only ---
dev="$(bash "$SCRIPT" --env dev --tag v9.9.9 --dry-run 2>&1)" || { echo "FAIL: dev dry-run exited non-zero" >&2; fail=1; }
has "dev pushes the -dev runner image" "$dev" "-t ghcr.io/falconiere/toolu-ghrunner:9.9.9-dev --push"
has "dev pushes the -dev docker image" "$dev" "-t ghcr.io/falconiere/toolu-ghrunner:9.9.9-dev-docker --push"
has "dev pushes the -dev macOS image" "$dev" "-t ghcr.io/falconiere/toolu-ghrunner-macos:9.9.9-dev --push"
has "dev builds darwin with the shared recipe" "$dev" "+ bash scripts/build-release.sh darwin arm64 dist/release"
has "dev cross-builds darwin/amd64" "$dev" "+ bash scripts/build-release.sh darwin amd64 dist/release x86_64-apple-darwin"
has "dev builds linux in the release rust image" "$dev" "rust:1.94.1-bookworm bash scripts/build-release.sh linux arm64 dist/release"
has "dev stages the image binaries from the tarballs" "$dev" "tar -xzf dist/release/toolu-runner-darwin-arm64.tar.gz -O toolu-runner > dist/toolu-runner-darwin-arm64"
lacks "dev creates no GitHub release" "$dev" "publish-release.sh"
lacks "dev leaves Homebrew alone" "$dev" "commit-homebrew-formula.sh"

# --- prod: version tags, release, Homebrew, gate; never :latest ---
prod="$(bash "$SCRIPT" --env prod --tag "v$version" --dry-run 2>&1)" || { echo "FAIL: prod dry-run exited non-zero" >&2; fail=1; }
has "prod pushes the versioned runner image" "$prod" "-t ghcr.io/falconiere/toolu-ghrunner:$version --push"
has "prod pushes the versioned docker image" "$prod" "-t ghcr.io/falconiere/toolu-ghrunner:$version-docker --push"
has "prod publishes with the shared recipe" "$prod" "+ env GH_REPO=Falconiere/toolu-ghrunner bash scripts/publish-release.sh v$version dist/release"
has "prod commits the Homebrew formula with the shared recipe" "$prod" "+ bash scripts/commit-homebrew-formula.sh v$version dist/release/SHA256SUMS"
lacks "prod re-runs no local gate (it requires CI green on HEAD)" "$prod" "tools/check.sh"
lacks "prod never moves :latest" "$prod" ":latest"
lacks "prod pushes no -dev tag" "$prod" "-dev"
has "prod notes come from CHANGELOG.md by default" "$prod" "notes: CHANGELOG.md [$version]"
cliff="$(bash "$SCRIPT" --env prod --tag "v$version" --change-log --dry-run 2>&1)"
has "--change-log notes come from git-cliff" "$cliff" "notes: git-cliff"

# --- guards ---
exits "prod tag must match the Cargo version" 1 --env prod --tag v0.0.0-nope
exits "prod tag needs a leading v" 2 --env prod --tag "$version"
exits "unknown env exits 2" 2 --env staging --tag v1.0.0
exits "missing tag exits 2" 2 --env dev
exits "tag with a slash exits 2" 2 --env dev --tag feat/x
exits "a tag that leaves an empty image tag exits 2" 2 --env dev --tag v
exits "a tag giving an image tag starting with '.' exits 2" 2 --env dev --tag v.x
exits "unknown option exits 2" 2 --env dev --tag v1 --bogus
exits "--change-log is prod-only" 2 --env dev --tag v1 --change-log

exit "$fail"
