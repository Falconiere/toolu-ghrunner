#!/usr/bin/env bash
# package-daemon-release.sh — assemble one release tarball for toolu-daemon,
# for a given os/arch.
#
# Mirrors scripts/package-release.sh's tarball layout for toolu-runner, minus
# the darwin leg: crates/daemon depends on bollard talking to a local Docker
# socket, so toolu-daemon ships for linux only.
#
# Produces <out_dir>/toolu-daemon-<os>-<arch>.tar.gz containing:
#     ./toolu-daemon                    (executable, 0755, at archive root)
#     ./scripts/toolu-daemon.service   (systemd unit — the linux-only story)
# The service file is copied from this script's own scripts/ dir, so the
# tarball ships the same unit scripts/test/daemon_systemd_test.sh validates.
#
# Usage:
#   package-daemon-release.sh <os> <arch> <bin_path> <out_dir>
#     <os>        must be 'linux' — toolu-daemon has no macOS build
#     <arch>      amd64 | arm64
#     <bin_path>  path to the built toolu-daemon binary
#     <out_dir>   directory to write the tarball into (created if absent)
#
# Prints the tarball path on success. Exits 2 on a missing/invalid arg or
# missing input.
set -euo pipefail

os="${1:-}"
arch="${2:-}"
bin_path="${3:-}"
out_dir="${4:-}"

if [[ -z "$os" || -z "$arch" || -z "$bin_path" || -z "$out_dir" ]]; then
  echo "package-daemon-release: usage: package-daemon-release.sh <os> <arch> <bin_path> <out_dir>" >&2
  exit 2
fi
if [[ "$os" != "linux" ]]; then
  echo "package-daemon-release: os '$os' unsupported — toolu-daemon ships for linux only (no Docker-on-macOS story)" >&2
  exit 2
fi
if [[ ! -f "$bin_path" ]]; then
  echo "package-daemon-release: binary '$bin_path' not found" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
bin_name="toolu-daemon"
unit="$script_dir/$bin_name.service"
if [[ ! -f "$unit" ]]; then
  echo "package-daemon-release: service file '$unit' not found" >&2
  exit 2
fi

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
tarball="$out_dir/$bin_name-$os-$arch.tar.gz"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

install -m 0755 "$bin_path" "$stage/$bin_name"
mkdir -p "$stage/scripts"
cp "$unit" "$stage/scripts/"

# Explicit members (not '.') so entries are 'toolu-daemon' and 'scripts/…'
# without a './' prefix, matching package-release.sh's convention.
tar -C "$stage" -czf "$tarball" "$bin_name" scripts

echo "$tarball"
