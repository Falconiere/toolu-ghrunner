#!/usr/bin/env bash
# Validate rendered user/system units with the installed systemd parser, then
# prove a transient user service inherits the PATH declared by the user unit.
set -euo pipefail

cd "$(dirname "$0")/../.."
command -v systemd-analyze >/dev/null || { echo 'systemd-analyze is required' >&2; exit 1; }
command -v systemd-run >/dev/null || { echo 'systemd-run is required' >&2; exit 1; }

fixtures=crates/config/tests/fixtures/service
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for scope in user root; do
  source="$fixtures/machine_plain.service"
  if [[ "$scope" == root ]]; then source="$fixtures/machine_root.service"; fi
  unit="$work/toolu-headless-$scope.service"
  # The renderer fixture names a synthetic binary; substitute only ExecStart
  # so systemd-analyze can verify the real directives without that binary.
  awk '/^ExecStart=/ { print "ExecStart=/usr/bin/env"; next } { print }' "$source" > "$unit"
  if [[ "$scope" == user ]]; then
    systemd-analyze --user verify "$unit"
  else
    systemd-analyze verify "$unit"
  fi
done

env_line=$(awk '/^Environment="PATH=/ { print; exit }' "$fixtures/machine_plain.service")
[[ -n "$env_line" ]] || { echo 'PATH missing from rendered fixture' >&2; exit 1; }
output=$(systemd-run --user --wait --pipe --collect --unit="toolu-headless-$$" \
  -p "$env_line" /usr/bin/env)
printf '%s\n' "$output" | grep -Fx 'PATH=/opt/dev tools & bin:/usr/bin:/bin' >/dev/null
echo 'headless_service_systemd passed'
