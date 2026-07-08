#!/usr/bin/env bash
# changelog-extract.sh — print the CHANGELOG body for one version.
#
# Used by the release workflow to turn the "## [X.Y.Z] - <date>" section of
# CHANGELOG.md into the GitHub Release notes. Prints the section body only
# (heading excluded, surrounding blank lines trimmed, stops at the next
# "## [" heading). A release must have notes, so an absent version is an error.
#
# Usage:
#   changelog-extract.sh <version> [changelog_path=CHANGELOG.md]
#     <version>   e.g. 0.1.0 (leading 'v' tolerated).
#
# Exit codes:
#   0  section found (body printed to stdout)
#   1  version has no section in the changelog
#   2  usage error (missing version / changelog file)
set -euo pipefail

version="${1:-}"
changelog="${2:-CHANGELOG.md}"

if [[ -z "$version" ]]; then
  echo "changelog-extract: missing <version> (usage: changelog-extract.sh <version> [changelog])" >&2
  exit 2
fi
if [[ ! -f "$changelog" ]]; then
  echo "changelog-extract: changelog '$changelog' not found" >&2
  exit 2
fi

version="${version#v}" # tolerate a leading 'v' from a tag name

if ! awk -v ver="$version" '
  BEGIN { pat = "^## \\[" ver "\\]"; found = 0; n = 0 }
  $0 ~ pat            { found = 1; insec = 1; next }
  insec && /^## \[/   { insec = 0 }
  insec               { body[++n] = $0 }
  END {
    if (!found) exit 3
    lo = 1; hi = n
    while (lo <= hi && body[lo] ~ /^[[:space:]]*$/) lo++
    while (hi >= lo && body[hi] ~ /^[[:space:]]*$/) hi--
    for (i = lo; i <= hi; i++) print body[i]
  }
' "$changelog"; then
  echo "changelog-extract: no section '## [$version]' in '$changelog'" >&2
  exit 1
fi
