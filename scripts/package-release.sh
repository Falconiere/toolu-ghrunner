#!/usr/bin/env bash
# package-release.sh — assemble one release tarball for a given os/arch.
#
# Produces <out_dir>/toolu-runner-<os>-<arch>.tar.gz with the exact layout
# install.sh expects (see install.sh binary + --service probes):
#     ./toolu-runner                   (executable, 0755, at archive root)
#     ./scripts/io.toolu-runner.plist  (launchd agent — darwin --service)
#     ./scripts/toolu-runner.service   (systemd unit  — linux  --service)
# The service files are copied from this script's own scripts/ dir, so the
# tarball ships the same units the repo tests validate.
#
# Usage:
#   package-release.sh <os> <arch> <bin_path> <out_dir>
#     <os>        install.sh os token  (darwin | linux)
#     <arch>      install.sh arch token (amd64 | arm64)
#     <bin_path>  path to the built toolu-runner binary
#     <out_dir>   directory to write the tarball into (created if absent)
#
# Prints the tarball path on success. Exits 2 on a missing arg / missing input.
set -euo pipefail

os="${1:-}"
arch="${2:-}"
bin_path="${3:-}"
out_dir="${4:-}"

if [[ -z "$os" || -z "$arch" || -z "$bin_path" || -z "$out_dir" ]]; then
  echo "package-release: usage: package-release.sh <os> <arch> <bin_path> <out_dir>" >&2
  exit 2
fi
if [[ ! -f "$bin_path" ]]; then
  echo "package-release: binary '$bin_path' not found" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
bin_name="toolu-runner"
plist="$script_dir/io.$bin_name.plist"
unit="$script_dir/$bin_name.service"
for f in "$plist" "$unit"; do
  if [[ ! -f "$f" ]]; then
    echo "package-release: service file '$f' not found" >&2
    exit 2
  fi
done

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
tarball="$out_dir/$bin_name-$os-$arch.tar.gz"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

install -m 0755 "$bin_path" "$stage/$bin_name"
mkdir -p "$stage/scripts"
cp "$plist" "$stage/scripts/"
cp "$unit" "$stage/scripts/"

# Explicit members (not '.') so entries are 'toolu-runner' and 'scripts/…'
# without a './' prefix, matching install.sh's root-level extraction probe.
tar -C "$stage" -czf "$tarball" "$bin_name" scripts

echo "$tarball"
