#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLIST="$SCRIPT_DIR/../io.toolu-runner.plist"

# xmllint validates the XML (if installed)
if command -v xmllint >/dev/null; then
  xmllint --noout "$PLIST"
fi

# plutil validates the plist (macOS only)
if command -v plutil >/dev/null; then
  plutil -lint "$PLIST"
fi

# Plist has the expected keys
grep -q '<key>Label</key>' "$PLIST"
grep -q '<key>ProgramArguments</key>' "$PLIST"
grep -q '<string>run</string>' "$PLIST"
grep -q '<string>--config</string>' "$PLIST"
grep -q '<key>KeepAlive</key>' "$PLIST"
grep -q '<key>RunAtLoad</key>' "$PLIST"

echo "plist_test passed"