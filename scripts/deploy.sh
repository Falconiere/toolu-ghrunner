#!/usr/bin/env bash
# deploy.sh — build every release artifact on this machine and push it,
# without waiting on CI. The local twin of release.yml + its chained
# release-image / release-macos-image / release-homebrew workflows.
#
#   build    darwin arm64 + amd64 (cargo, native + x86_64-apple-darwin),
#            linux amd64 + arm64 (cargo inside rust:1.94.1-bookworm, the
#            image release.yml's linux legs use: same glibc floor), then the
#            install.sh tarballs + daemon tarballs + SHA256SUMS in dist/release/
#   push     --env dev   images only, every tag suffixed -dev:
#                          ghcr.io/falconiere/toolu-ghrunner:<v>-dev (+ -dev-docker)
#                          ghcr.io/falconiere/toolu-ghrunner-macos:<v>-dev
#            --env prod  images at :<v> (+ :<v>-docker, macOS :<v>) — this
#                        script never pushes :latest / :vN — then the GitHub
#                        Release (prerelease when the tag has '-') and, for a
#                        stable tag, the Homebrew formula in Falconiere/homebrew-tap.
#                        Creating the release creates the tag, which can start
#                        release.yml (push: tags v*): CI then re-pushes the
#                        images (incl. :latest / :vN) and its publish step fails
#                        because the release already exists.
#
# prod also requires: tag == Cargo version, a clean tree whose HEAD is on
# origin/main, a CHANGELOG.md section for the version (the release notes), no
# existing release, and a green ./tools/check.sh all; it asks you to type the
# tag before pushing. --change-log instead writes the notes from the commits
# since the last tag with git-cliff (cliff.toml); CHANGELOG.md is not edited.
# <v> is the tag minus its leading 'v'. Images are pushed by tag; the
# digest-pinned refs toolu.sh needs are printed at the end.
#
# Usage:
#   scripts/deploy.sh --env dev  --tag v0.13.0
#   scripts/deploy.sh --env prod --tag v0.13.0 [--yes] [--change-log]
#   scripts/deploy.sh --env prod --tag v0.13.0 --dry-run   # print, run nothing
#
# Requires: an Apple Silicon Mac (darwin binaries need Apple's SDK), cargo +
# rustup, Docker with buildx and multi-platform push (Docker Desktop), `docker
# login ghcr.io` with write:packages, `gh auth login` (prod), and git-cliff
# (--change-log).
set -euo pipefail

REPO="Falconiere/toolu-ghrunner"
TAP_REPO="Falconiere/homebrew-tap"
IMAGE="ghcr.io/falconiere/toolu-ghrunner"
MACOS_IMAGE="ghcr.io/falconiere/toolu-ghrunner-macos"
RUST_IMAGE="rust:1.94.1-bookworm"

ENV=""
TAG=""
YES=0
DRY_RUN=0
CHANGE_LOG=0

usage() {
  sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
}

die() {
  echo "deploy: $*" >&2
  exit 2
}

# Echo every command; run it unless --dry-run.
run() {
  echo "+ $*"
  [[ $DRY_RUN -eq 1 ]] || "$@"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --env) ENV="${2:-}"; shift 2 || die "--env needs prod or dev" ;;
    --tag) TAG="${2:-}"; shift 2 || die "--tag needs a value" ;;
    --yes) YES=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --change-log) CHANGE_LOG=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option '$1' (see --help)" ;;
  esac
done

[[ $ENV == prod || $ENV == dev ]] || die "--env must be prod or dev"
[[ -n $TAG ]] || die "--tag is required (e.g. v0.13.0)"
[[ $CHANGE_LOG -eq 0 || $ENV == prod ]] || die "--change-log writes release notes; only --env prod cuts a release"
# Docker's tag charset; it also keeps the tag safe inside every command below.
[[ $TAG =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,100}$ ]] || die "tag '$TAG' is not a valid image tag"

cd "$(dirname "$0")/.."
version="${TAG#v}"
rel="dist/release"

if [[ $ENV == prod ]]; then
  [[ $TAG == v* ]] || die "a prod tag starts with 'v' (e.g. v$version)"
  bash scripts/assert-version.sh "$TAG" || exit 1
  suffix=""
else
  suffix="-dev"
fi
runner_ref="$IMAGE:$version$suffix"
docker_ref="$IMAGE:$version$suffix-docker"
macos_ref="$MACOS_IMAGE:$version$suffix"

echo "==> deploy $TAG to $ENV"
echo "    images: $runner_ref, $docker_ref, $macos_ref"
if [[ $ENV == prod ]]; then
  echo "    GitHub release $TAG on $REPO$([[ $TAG == *-* ]] && echo ' (prerelease, no Homebrew)' || echo " + Homebrew formula in $TAP_REPO")"
  echo "    notes: $([[ $CHANGE_LOG -eq 1 ]] && echo "git-cliff, commits since the last tag" || echo "CHANGELOG.md [$version]")"
fi

notes="$(mktemp)"
trap 'rm -f "$notes"' EXIT
if [[ $DRY_RUN -eq 0 ]]; then
  [[ "$(uname -s)/$(uname -m)" == Darwin/arm64 ]] \
    || die "needs an Apple Silicon Mac: the darwin binaries cannot be built elsewhere"
  for tool in cargo rustup docker git shasum; do
    command -v "$tool" >/dev/null 2>&1 || die "'$tool' not found"
  done
  docker buildx version >/dev/null 2>&1 || die "docker buildx is not available"
  if [[ $ENV == prod ]]; then
    command -v gh >/dev/null 2>&1 || die "'gh' not found"
    [[ -z "$(git status --porcelain)" ]] || die "working tree is dirty; prod deploys a committed tree"
    git fetch -q origin main
    git merge-base --is-ancestor HEAD origin/main || die "HEAD is not on origin/main; merge it first"
    if [[ $CHANGE_LOG -eq 1 ]]; then
      command -v git-cliff >/dev/null 2>&1 || die "'git-cliff' not found (--change-log)"
      git fetch -q --tags origin
      # Same section release-pr.sh would prepend, minus its "## [x.y.z]" heading.
      git-cliff --unreleased --tag "$TAG" --strip all | sed '/^## \[/d' >"$notes"
      [[ -s $notes ]] || die "git-cliff found no releasable commits since the last tag"
    else
      bash scripts/changelog-extract.sh "$version" >"$notes" \
        || die "CHANGELOG.md has no [$version] section (or pass --change-log)"
    fi
    echo "==> release notes"
    cat "$notes"
    # shortcut: no resume — a run that created the release but failed on
    # Homebrew stops here on retry; push the tap by hand (steps below), or add
    # a --skip-release flag if this recurs.
    if gh release view "$TAG" -R "$REPO" >/dev/null 2>&1; then
      die "release $TAG already exists on $REPO (to finish a failed Homebrew step: gh release download $TAG -R $REPO -p SHA256SUMS, then scripts/generate-homebrew-formula.sh $TAG SHA256SUMS > Formula/toolu-runner.rb in a $TAP_REPO clone, commit, push)"
    fi
  fi
fi

if [[ $ENV == prod ]]; then
  echo "==> gate"
  run ./tools/check.sh all
fi

echo "==> build"
run mkdir -p "$rel"
run rustup target add x86_64-apple-darwin
for arch in arm64 amd64; do
  triple="aarch64-apple-darwin"
  [[ $arch == amd64 ]] && triple="x86_64-apple-darwin"
  run cargo build --release --locked --bin toolu-runner --target "$triple"
  run install -m 0755 "target/$triple/release/toolu-runner" "dist/toolu-runner-darwin-$arch"
done
for arch in amd64 arm64; do
  run docker run --rm --platform "linux/$arch" -v "$PWD:/src" -w /src \
    -v toolu-deploy-cargo-registry:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR="/src/target/linux-$arch" \
    "$RUST_IMAGE" cargo build --release --locked --bin toolu-runner --bin toolu-daemon
  run install -m 0755 "target/linux-$arch/release/toolu-runner" "dist/toolu-runner-linux-$arch"
  run install -m 0755 "target/linux-$arch/release/toolu-daemon" "dist/toolu-daemon-linux-$arch"
done

echo "==> package"
for target in darwin-arm64 darwin-amd64 linux-amd64 linux-arm64; do
  run bash scripts/package-release.sh "${target%-*}" "${target#*-}" "dist/toolu-runner-$target" "$rel"
done
for arch in amd64 arm64; do
  run bash scripts/package-daemon-release.sh linux "$arch" "dist/toolu-daemon-linux-$arch" "$rel"
done
echo "+ (cd $rel && shasum -a 256 *.tar.gz > SHA256SUMS)"
if [[ $DRY_RUN -eq 0 ]]; then
  (cd "$rel" && shasum -a 256 ./*.tar.gz | sed 's|  \./|  |' >SHA256SUMS)
fi

if [[ $ENV == prod && $YES -eq 0 && $DRY_RUN -eq 0 ]]; then
  printf 'Publish %s to prod (images, GitHub release, Homebrew)? Type the tag to confirm: ' "$TAG"
  read -r answer </dev/tty || die "no terminal to confirm on; pass --yes"
  [[ $answer == "$TAG" ]] || die "not confirmed; nothing was pushed"
fi

echo "==> push images"
run docker buildx build --platform linux/amd64,linux/arm64 --build-arg RUNNER_BINARY=prebuilt \
  --target runner -t "$runner_ref" --push .
run docker buildx build --platform linux/amd64,linux/arm64 --build-arg RUNNER_BINARY=prebuilt \
  --target docker -t "$docker_ref" --push .
run docker buildx build --platform linux/arm64 -f Dockerfile.macos -t "$macos_ref" --push .

if [[ $ENV == prod ]]; then
  echo "==> GitHub release"
  prerelease=()
  [[ $TAG == *-* ]] && prerelease=(--prerelease)
  run gh release create "$TAG" -R "$REPO" --target "$(git rev-parse HEAD)" --title "$TAG" \
    --notes-file "$notes" ${prerelease[@]+"${prerelease[@]}"} "$rel"/*.tar.gz "$rel/SHA256SUMS"

  if [[ $TAG != *-* ]]; then
    echo "==> Homebrew formula"
    tap="$(mktemp -d)"
    trap 'rm -f "$notes"; rm -rf "$tap"' EXIT
    run gh repo clone "$TAP_REPO" "$tap" -- --depth 1
    echo "+ scripts/generate-homebrew-formula.sh $TAG $rel/SHA256SUMS > $tap/Formula/toolu-runner.rb"
    if [[ $DRY_RUN -eq 0 ]]; then
      mkdir -p "$tap/Formula"
      bash scripts/generate-homebrew-formula.sh "$TAG" "$rel/SHA256SUMS" >"$tap/Formula/toolu-runner.rb"
    fi
    run git -C "$tap" add Formula/toolu-runner.rb
    run git -C "$tap" commit -m "toolu-runner $TAG"
    run git -C "$tap" push
  fi
fi

echo "==> done: digest-pinned refs (pin these in toolu.sh, never the tag)"
for ref in "$runner_ref" "$docker_ref" "$macos_ref"; do
  if [[ $DRY_RUN -eq 1 ]]; then
    echo "    ${ref%:*}@sha256:<digest of :${ref##*:}>"
  else
    echo "    ${ref%:*}@$(docker buildx imagetools inspect "$ref" | awk '/^Digest:/ {print $2; exit}')"
  fi
done
