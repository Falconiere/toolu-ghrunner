#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVICE="$SCRIPT_DIR/../toolu-runner.service"

# systemd-analyze verify validates the unit (if available)
if command -v systemd-analyze >/dev/null; then
  systemd-analyze verify "$SERVICE"
fi

# Service has the expected sections
grep -q '^\[Unit\]$' "$SERVICE"
grep -q '^\[Service\]$' "$SERVICE"
grep -q '^\[Install\]$' "$SERVICE"
grep -q '^ExecStart=/usr/local/bin/toolu-runner ' "$SERVICE"
grep -q '^Restart=always$' "$SERVICE"
grep -q '^WantedBy=multi-user.target$' "$SERVICE"

echo "systemd_test passed"