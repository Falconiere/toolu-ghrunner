#!/usr/bin/env bash
# daemon_systemd_test.sh — static validation of scripts/toolu-daemon.service.
#
# Modeled on systemd_test.sh, but deliberately does NOT run `systemd-analyze
# verify` against the real file: it also checks that ExecStart's binary
# exists on disk, and nothing installs toolu-daemon to /usr/local/bin in a
# clean checkout — that is the exact reason systemd_test.sh itself is not
# registered in ci.yml (see the comment there). Instead this verifies a copy
# of the unit with ExecStart rewritten to a stub binary this test creates —
# every other directive stays byte-identical to what ships, so a genuinely
# malformed unit still fails, but a missing toolu-daemon build never does.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVICE="$SCRIPT_DIR/../toolu-daemon.service"

if [[ ! -f "$SERVICE" ]]; then
  echo "FAIL: $SERVICE not found" >&2
  exit 1
fi

if command -v systemd-analyze >/dev/null; then
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT

  exec_path=$(grep -E '^ExecStart=' "$SERVICE" | head -1 | cut -d= -f2-)
  if [[ -z "$exec_path" ]]; then
    echo "FAIL: $SERVICE has no ExecStart=" >&2
    exit 1
  fi

  stub="$work/toolu-daemon"
  printf '#!/bin/sh\nexit 0\n' >"$stub"
  chmod +x "$stub"

  verify_copy="$work/toolu-daemon.service"
  sed "s#^ExecStart=.*#ExecStart=$stub#" "$SERVICE" >"$verify_copy"
  systemd-analyze verify "$verify_copy"
fi

# Service has the expected sections
grep -q '^\[Unit\]$' "$SERVICE"
grep -q '^\[Service\]$' "$SERVICE"
grep -q '^\[Install\]$' "$SERVICE"
grep -q '^ExecStart=/usr/local/bin/toolu-daemon$' "$SERVICE"
grep -q '^WantedBy=multi-user.target$' "$SERVICE"

# A restart policy — the daemon must come back on its own (crash, host
# reboot, token rotation).
grep -q '^Restart=always$' "$SERVICE"
grep -q '^RestartSec=' "$SERVICE"

# Docker socket access: the service user must be able to reach it, so the
# unit is not sandboxed away from the docker group.
grep -q '^SupplementaryGroups=.*docker' "$SERVICE"

# TOOLU_DAEMON_TOKEN_FILE and the rest of the env contract
# (crates/daemon/src/config.rs) are documented on the unit, not left for an
# operator to guess.
for var in TOOLU_DAEMON_TOKEN_FILE TOOLU_DAEMON_BIND TOOLU_DAEMON_VCPU \
  TOOLU_DAEMON_MEMORY_MB TOOLU_DAEMON_QUEUE_MAX TOOLU_DAEMON_RUNTIME \
  TOOLU_DAEMON_IMAGE; do
  if ! grep -q "$var" "$SERVICE"; then
    echo "FAIL: $SERVICE does not document $var" >&2
    exit 1
  fi
done

# The token-file/console compat rule (line 1 current, optional line 2
# previous) is stated on the unit, not only in crates/daemon source.
grep -qi 'line 1 is the' "$SERVICE"
grep -qi 'previous token' "$SERVICE"

# EnvironmentFile is required (no leading '-'): the daemon has no config
# file to fall back to, so a missing env file must be a startup error, not
# a silent empty-env launch.
if grep -qE '^EnvironmentFile=-' "$SERVICE"; then
  echo "FAIL: $SERVICE's EnvironmentFile is optional ('-' prefix) — the daemon has no fallback config, so it must be required" >&2
  exit 1
fi
grep -qE '^EnvironmentFile=/' "$SERVICE"

# No ReadWritePaths: the daemon persists nothing to disk (state lives in
# Docker; crates/daemon/README.md). A ReadWritePaths line here would be a
# silent copy-paste regression from toolu-runner.service, so its absence is
# asserted, not merely undocumented.
if grep -q '^ReadWritePaths=' "$SERVICE"; then
  echo "FAIL: $SERVICE declares ReadWritePaths — the daemon writes nothing to disk" >&2
  exit 1
fi

echo "daemon_systemd_test passed"
