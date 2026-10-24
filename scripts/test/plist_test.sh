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

# The rendered machine LaunchAgent (#208) must be a valid plist too: lint the
# committed renderer fixtures (byte-pinned by crates/config/tests).
FIXTURES="$SCRIPT_DIR/../../crates/config/tests/fixtures/service"
for fixture in "$FIXTURES"/machine_*.plist; do
  if command -v xmllint >/dev/null; then
    xmllint --noout "$fixture"
  fi
  if command -v plutil >/dev/null; then
    plutil -lint "$fixture"
  fi
  grep -q '<string>sh.toolu.runner</string>' "$fixture"
  grep -q '<string>--all</string>' "$fixture"
done

echo "plist_test passed"