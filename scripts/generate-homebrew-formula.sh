#!/usr/bin/env bash
# generate-homebrew-formula.sh — render Formula/toolu-runner.rb for a release.
#
# Reads the 4-target SHA256SUMS emitted by the release workflow and prints a
# self-contained Homebrew formula that selects the right prebuilt tarball via
# on_macos/on_linux + on_arm/on_intel at install time. This repo builds
# outside cargo-dist (see docs/toolu/specs/2026-07-08-release-automation-design.md),
# so the formula is hand-authored instead of tool-generated.
#
# Usage:
#   generate-homebrew-formula.sh <tag> <sha256sums_path>
#     <tag>              e.g. v0.1.0 (leading 'v' required — matches the
#                        release asset URLs)
#     <sha256sums_path>  path to a SHA256SUMS file (sha256sum(1) format)
#                        covering all 4 toolu-runner-<os>-<arch>.tar.gz assets
#
# Exit codes:
#   0  formula printed to stdout
#   1  a target's checksum is missing from sha256sums_path
#   2  usage error
set -euo pipefail

tag="${1:-}"
sums="${2:-}"

if [[ -z "$tag" || -z "$sums" ]]; then
  echo "generate-homebrew-formula: usage: generate-homebrew-formula.sh <tag> <sha256sums_path>" >&2
  exit 2
fi
if [[ ! -f "$sums" ]]; then
  echo "generate-homebrew-formula: sha256sums file '$sums' not found" >&2
  exit 2
fi
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[-a-zA-Z0-9.]+)?$ ]]; then
  echo "generate-homebrew-formula: tag '$tag' is not a valid vX.Y.Z[-pre] tag" >&2
  exit 2
fi

version="${tag#v}"
repo="Falconiere/toolu-ghrunner"

sha_for() {
  local asset="$1" hash
  # SHA256SUMS lines are "<hash>  <filename>"; sha256sum -c also tolerates a
  # leading '*' on the filename for binary mode, so match either form.
  hash="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1; exit }' "$sums")"
  if [[ -z "$hash" ]]; then
    echo "generate-homebrew-formula: no checksum for '$asset' in '$sums'" >&2
    exit 1
  fi
  echo "$hash"
}

darwin_arm64_sha="$(sha_for "toolu-runner-darwin-arm64.tar.gz")"
darwin_amd64_sha="$(sha_for "toolu-runner-darwin-amd64.tar.gz")"
linux_amd64_sha="$(sha_for "toolu-runner-linux-amd64.tar.gz")"
linux_arm64_sha="$(sha_for "toolu-runner-linux-arm64.tar.gz")"

cat <<RUBY
class TooluRunner < Formula
  desc "Standalone self-hosted GitHub Actions JIT runner"
  homepage "https://github.com/${repo}"
  version "${version}"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/${repo}/releases/download/${tag}/toolu-runner-darwin-arm64.tar.gz"
      sha256 "${darwin_arm64_sha}"
    end
    on_intel do
      url "https://github.com/${repo}/releases/download/${tag}/toolu-runner-darwin-amd64.tar.gz"
      sha256 "${darwin_amd64_sha}"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/${repo}/releases/download/${tag}/toolu-runner-linux-amd64.tar.gz"
      sha256 "${linux_amd64_sha}"
    end
    on_arm do
      url "https://github.com/${repo}/releases/download/${tag}/toolu-runner-linux-arm64.tar.gz"
      sha256 "${linux_arm64_sha}"
    end
  end

  def install
    bin.install "toolu-runner"
    # launchd plist + systemd unit — optional, only used by --service installs.
    pkgshare.install "scripts" if File.directory?("scripts")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/toolu-runner --version")
  end
end
RUBY
