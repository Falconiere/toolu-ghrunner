#!/usr/bin/env bash
# release_homebrew_workflow_test.sh — static validation of
# .github/workflows/release-homebrew.yml.
#
# A published-release event can't be exercised offline, so this asserts the
# invariants the homebrew-publish contract depends on: trigger, prerelease
# skip, least-privilege permissions, checksum download, formula generation,
# and a guarded push to the external tap. Grep tier (dependency-free); if
# PyYAML is importable it additionally asserts the file parses.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/release-homebrew.yml"
fail=0

if [[ ! -f "$WF" ]]; then
  echo "FAIL: $WF not found" >&2
  exit 1
fi

want() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found: $pat" >&2
    fail=1
  fi
}

want "triggers on release published"      "types: \[published\]"
want "skips prereleases"                  "!contains\(github\.event\.release\.tag_name, '-'\)"
want "least-privilege permissions"        "^permissions:"
want "contents: read only"                "contents: read"
want "downloads SHA256SUMS from release"  "gh release download"
want "generates the formula via script"   "generate-homebrew-formula\.sh"
want "guards against a missing PAT"       'if \[\[ -z "\$TAP_TOKEN" \]\]'
want "pushes to the homebrew-tap repo"    "Falconiere/homebrew-tap"
want "skips an unchanged formula"         "git diff --quiet"
want "commits with the tag in the message" 'git commit -m "toolu-runner \$\{TAG\}"'

if python3 -c 'import yaml' >/dev/null 2>&1; then
  if python3 - "$WF" <<'PY'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf.get("jobs", {})
assert set(jobs) == {"publish-formula"}, f"jobs: {list(jobs)}"
assert wf["permissions"]["contents"] == "read"
print("ok: PyYAML deep-check (job set + read-only perm)")
PY
  then :; else
    echo "FAIL: PyYAML deep-check failed" >&2
    fail=1
  fi
else
  echo "# PyYAML unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "release_homebrew_workflow_test: FAILED" >&2
  exit 1
fi
echo "release_homebrew_workflow_test: all passed"
