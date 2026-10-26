#!/usr/bin/env bash
# deploy.sh — build every release artifact on this machine and push it,
# without waiting on CI. The local twin of release.yml + its chained
# release-image / release-macos-image / release-homebrew workflows, sharing
# their recipes: scripts/build-release.sh, scripts/publish-release.sh,
# scripts/commit-homebrew-formula.sh.
#
#   build    darwin arm64 + amd64 on this Mac (amd64 cross-built), linux
#            amd64 + arm64 inside rust:1.94.1-bookworm (release.yml's linux
#            image: same glibc floor) — tarballs + daemon tarballs in dist/release/
#   push     --env dev   images only, every tag suffixed -dev:
#                          ghcr.io/falconiere/toolu-ghrunner:<v>-dev (+ -dev-docker)
#                          ghcr.io/falconiere/toolu-ghrunner-macos:<v>-dev
#            --env prod  images at :<v> (+ :<v>-docker, macOS :<v>) — never
#                        :latest / :vN — then the GitHub Release (prerelease
#                        when the tag has '-') and, for a stable tag, the
#                        Homebrew formula in Falconiere/homebrew-tap. Creating
#                        the release creates the tag; release.yml sees the
#                        release already exists and skips its own run.
#
# prod also requires: tag == Cargo version, the tag absent on origin, a clean
# tree whose HEAD is on origin/main with a passing ci.yml run, a CHANGELOG.md
# section for the version (the release notes), push access to the tap; it
# asks you to type the tag before pushing. --change-log instead writes the
# notes from the commits since the last tag with git-cliff (cliff.toml);
# CHANGELOG.md is not edited. <v> is the tag minus its leading 'v'. The
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
# Keeps the tag safe inside every command below.
[[ $TAG =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,100}$ ]] || die "tag '$TAG' may only hold letters, digits, '.', '_' and '-'"

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
# Docker's tag grammar, checked now rather than after an hour of building.
[[ $version$suffix =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]] \
  || die "tag '$TAG' gives image tag ':$version$suffix', which Docker rejects"
runner_ref="$IMAGE:$version$suffix"
docker_ref="$IMAGE:$version$suffix-docker"
macos_ref="$MACOS_IMAGE:$version$suffix"
stable_prod=0
[[ $ENV == prod && $TAG != *-* ]] && stable_prod=1

echo "==> deploy $TAG to $ENV"
echo "    images: $runner_ref, $docker_ref, $macos_ref"
if [[ $ENV == prod ]]; then
  echo "    GitHub release $TAG on $REPO$([[ $stable_prod -eq 1 ]] && echo " + Homebrew formula in $TAP_REPO" || echo ' (prerelease, no Homebrew)')"
  echo "    notes: $([[ $CHANGE_LOG -eq 1 ]] && echo "git-cliff, commits since the last tag" || echo "CHANGELOG.md [$version]")"
fi

notes="$(mktemp)"
tap="$(mktemp -d)"
trap 'rm -f "$notes"; rm -rf "$tap"' EXIT
if [[ $DRY_RUN -eq 0 ]]; then
  [[ "$(uname -s)/$(uname -m)" == Darwin/arm64 ]] \
    || die "needs an Apple Silicon Mac: the darwin binaries cannot be built elsewhere"
  for tool in cargo rustup docker git; do
    command -v "$tool" >/dev/null 2>&1 || die "'$tool' not found"
  done
  docker buildx version >/dev/null 2>&1 || die "docker buildx is not available"
  if [[ $ENV == prod ]]; then
    command -v gh >/dev/null 2>&1 || die "'gh' not found"
    [[ -z "$(git status --porcelain)" ]] || die "working tree is dirty; prod deploys a committed tree"
    git fetch -q origin main
    git merge-base --is-ancestor HEAD origin/main || die "HEAD is not on origin/main; merge it first"
    head="$(git rev-parse HEAD)"
    # The tag (and with it any release) must not exist yet: release-tag.sh may
    # already have pushed it, and release.yml then owns this version.
    # shortcut: no resume — a deploy that created the release but failed on
    # Homebrew also stops here on retry; the message gives the manual steps.
    if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null; then
      die "tag $TAG already exists on origin; release.yml owns it. (Finishing a deploy whose Homebrew step failed: gh release download $TAG -R $REPO -p SHA256SUMS -D <dir>; scripts/commit-homebrew-formula.sh $TAG <dir>/SHA256SUMS <$TAP_REPO clone>; git -C <clone> push)"
    fi
    # The gate CI already ran on this commit, on Linux and macOS, instead of
    # a local re-run that would only cover this Mac.
    passed="$(gh run list -R "$REPO" --workflow ci.yml --commit "$head" --status success --json databaseId -q length)"
    [[ $passed -gt 0 ]] || die "ci.yml has not passed on $head yet (gh run list -R $REPO --workflow ci.yml --commit $head)"
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
    if [[ $stable_prod -eq 1 ]]; then
      # Prove the tap push works now: after the release exists it is too late.
      gh repo clone "$TAP_REPO" "$tap" -- --depth 1 -q
      [[ -n "$(git -C "$tap" config user.email)" ]] || die "git has no user.email for the $TAP_REPO commit"
      git -C "$tap" push --dry-run -q || die "cannot push to $TAP_REPO (gh auth setup-git, or check access)"
    fi
  fi
fi

echo "==> build"
run mkdir -p "$rel"
run rustup target add x86_64-apple-darwin
run bash scripts/build-release.sh darwin arm64 "$rel"
run bash scripts/build-release.sh darwin amd64 "$rel" x86_64-apple-darwin
for arch in amd64 arm64; do
  run docker run --rm --platform "linux/$arch" -v "$PWD:/src" -w /src \
    -v toolu-deploy-cargo-registry:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR="/src/target/linux-$arch" \
    "$RUST_IMAGE" bash scripts/build-release.sh linux "$arch" "$rel"
done
# The image builds read the binaries out of the tarballs, as release-image.yml
# and release-macos-image.yml do: the images ship the released bytes.
for target in linux-amd64 linux-arm64 darwin-arm64; do
  echo "+ tar -xzf $rel/toolu-runner-$target.tar.gz -O toolu-runner > dist/toolu-runner-$target"
  if [[ $DRY_RUN -eq 0 ]]; then
    tar -xzf "$rel/toolu-runner-$target.tar.gz" -O toolu-runner >"dist/toolu-runner-$target"
    chmod 0755 "dist/toolu-runner-$target"
  fi
done

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
  run env GH_REPO="$REPO" bash scripts/publish-release.sh "$TAG" "$rel" "$notes" "$(git rev-parse HEAD)"
  if [[ $stable_prod -eq 1 ]]; then
    echo "==> Homebrew formula"
    run bash scripts/commit-homebrew-formula.sh "$TAG" "$rel/SHA256SUMS" "$tap"
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
