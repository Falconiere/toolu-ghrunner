#!/usr/bin/env bash
# build-release.sh — build and package one release target: the toolu-runner
# tarball, plus the toolu-daemon tarball on linux (it has no macOS build).
#
# The one build recipe for release.yml's `build` matrix and scripts/deploy.sh.
# Runs on the host it builds for: release.yml's darwin legs on the macOS
# runner, its linux legs inside rust:1.94.1-bookworm (the Dockerfile builder's
# image, so the glibc floor matches the runtime image). deploy.sh does the same.
#
# Usage:
#   build-release.sh <os> <arch> <out_dir>
#     <os>       darwin | linux
#     <arch>     amd64 | arm64
#     <out_dir>  where the tarballs go (created if absent; relative to the
#                repo root, which the script runs from)
#
# Honours CARGO_TARGET_DIR. Exits 2 on a usage error.
set -euo pipefail

os="${1:-}"
arch="${2:-}"
out_dir="${3:-}"

if [[ -z "$os" || -z "$arch" || -z "$out_dir" ]]; then
  echo "build-release: usage: build-release.sh <os> <arch> <out_dir>" >&2
  exit 2
fi

cd "$(dirname "$0")/.."
bin_dir="${CARGO_TARGET_DIR:-target}/release"

# --locked pins the committed Cargo.lock.
cargo build --release --locked --bin toolu-runner
bash scripts/package-release.sh "$os" "$arch" "$bin_dir/toolu-runner" "$out_dir"

if [[ "$os" == linux ]]; then
  cargo build --release --locked --bin toolu-daemon
  bash scripts/package-daemon-release.sh "$os" "$arch" "$bin_dir/toolu-daemon" "$out_dir"
fi
