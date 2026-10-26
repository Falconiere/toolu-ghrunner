#!/usr/bin/env bash
# deploy_test.sh — scripts/deploy.sh: the refs a release pushes and what it
# never touches, the argument guards (both --dry-run), and the Docker
# preflight (stubbed). Nothing is built or pushed.
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

# --- a release: version tags, GitHub release, Homebrew; never :latest ---
rel="$(bash "$SCRIPT" --tag "v$version" --dry-run 2>&1)" || { echo "FAIL: dry-run exited non-zero" >&2; fail=1; }
has "pushes the versioned runner image" "$rel" "-t ghcr.io/falconiere/toolu-ghrunner:$version --push"
has "pushes the versioned docker image" "$rel" "-t ghcr.io/falconiere/toolu-ghrunner:$version-docker --push"
has "pushes the versioned macOS image" "$rel" "-t ghcr.io/falconiere/toolu-ghrunner-macos:$version --push"
has "builds darwin with the shared recipe" "$rel" "+ bash scripts/build-release.sh darwin arm64 dist/release"
has "cross-builds darwin/amd64" "$rel" "+ bash scripts/build-release.sh darwin amd64 dist/release x86_64-apple-darwin"
has "builds linux in the release rust image" "$rel" "rust:1.94.1-bookworm bash scripts/build-release.sh linux arm64 dist/release"
has "stages the image binaries from the tarballs" "$rel" "tar -xzf dist/release/toolu-runner-darwin-arm64.tar.gz -O toolu-runner > dist/toolu-runner-darwin-arm64"
has "publishes with the shared recipe" "$rel" "+ env GH_REPO=Falconiere/toolu-ghrunner bash scripts/publish-release.sh v$version dist/release"
has "commits the Homebrew formula with the shared recipe" "$rel" "+ bash scripts/commit-homebrew-formula.sh v$version dist/release/SHA256SUMS"
has "refreshes the tap clone before committing" "$rel" "pull -q --ff-only"
has "starts from an empty dist/release" "$rel" "+ rm -rf dist/release"
has "stamps the OCI version label" "$rel" "--label org.opencontainers.image.version=v$version"
lacks "runs no test gate" "$rel" "tools/check.sh"
has "labels the images with the HEAD commit" "$rel" "--label org.opencontainers.image.revision=$(git rev-parse HEAD)"
lacks "never moves :latest" "$rel" ":latest"
lacks "pushes no -dev tag" "$rel" "-dev"
has "notes come from CHANGELOG.md by default" "$rel" "notes: CHANGELOG.md [$version]"
cliff="$(bash "$SCRIPT" --tag "v$version" --change-log --dry-run 2>&1)"
has "--change-log notes come from git-cliff" "$cliff" "notes: git-cliff"

# --- guards ---
exits "the tag must match the Cargo version" 1 --tag v0.0.0-nope
exits "the tag needs a leading v" 2 --tag "$version"
exits "missing tag exits 2" 2
exits "tag with a slash exits 2" 2 --tag feat/x
exits "unknown option (the removed --env) exits 2" 2 --env dev --tag "v$version"

# --- preflight: a Docker VM that cannot see the repo stops before any build ---
# Stubs: an Apple Silicon host, and a docker whose bind mount is empty (Colima
# outside $HOME) — `docker run` fails, everything else succeeds.
stub="$(mktemp -d)"
trap 'rm -rf "$stub"' EXIT
CALLS="$stub/calls"
export CALLS
cat >"$stub/uname" <<'STUB'
#!/bin/sh
case "$1" in -s) echo Darwin ;; -m) echo arm64 ;; esac
STUB
cat >"$stub/cargo" <<'STUB'
#!/bin/sh
echo "$(basename "$0") $*" >>"$CALLS"
[ "$1" != run ] || [ -n "${DOCKER_SEES:-}" ]
STUB
cp "$stub/cargo" "$stub/rustup"
cp "$stub/cargo" "$stub/docker"
chmod +x "$stub"/*
unseen="$(PATH="$stub:$PATH" bash "$SCRIPT" --tag "v$version" 2>&1)"
got=$?
if [[ $got -eq 2 ]]; then echo "ok: an unshared repo path exits 2"; else echo "FAIL: an unshared repo path exits 2 (exit $got)" >&2; fail=1; fi
has "an unshared repo path names the Colima fix" "$unseen" "Docker cannot see $PWD"
has "the preflight mounts the repo" "$(cat "$CALLS")" "docker run --rm -v $PWD:/src rust:1.94.1-bookworm test -f /src/Cargo.toml"
lacks "an unshared repo path builds nothing" "$unseen" "==> build"
lacks "an unshared repo path never runs rustup" "$(cat "$CALLS")" "rustup"

# --- preflight: a cargo that is not rust-toolchain.toml's (the stub prints no
# version, like a Homebrew cargo ahead of ~/.cargo/bin) stops before any build ---
: >"$CALLS"
unpinned="$(DOCKER_SEES=1 PATH="$stub:$PATH" bash "$SCRIPT" --tag "v$version" 2>&1)"
got=$?
if [[ $got -eq 2 ]]; then echo "ok: an unpinned cargo exits 2"; else echo "FAIL: an unpinned cargo exits 2 (exit $got)" >&2; fail=1; fi
has "an unpinned cargo names the toolchain" "$unpinned" "cargo is not the pinned"
lacks "an unpinned cargo builds nothing" "$unpinned" "==> build"

exit "$fail"
