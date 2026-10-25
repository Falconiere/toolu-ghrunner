#!/usr/bin/env bash
# gitleaks_ignore_test.sh — static validation of .gitleaksignore.
#
# The CI review action's secret scan (gitleaks, its own pinned config) reads
# .gitleaksignore from the repo root and skips every fingerprint listed there.
# A fingerprint is `<path>:<rule-id>:<line>`, so it is line-anchored: when a
# fixture moves or a flagged line shifts, the entry stops matching and the
# finding silently returns on the next PR, while the stale line keeps looking
# like coverage. This pins that every entry still names a tracked file and a
# line inside it, and that no entry reaches outside a tests/ tree — the file
# may only ever exempt test fixtures, never production code.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

# GITLEAKS_IGNORE_FILE lets a negative check point the test at a mutated copy.
IGNORE="${GITLEAKS_IGNORE_FILE:-.gitleaksignore}"
fail=0

if [[ ! -f "$IGNORE" ]]; then
  echo "FAIL: $IGNORE not found" >&2
  exit 1
fi

entries=0
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  entries=$((entries + 1))
  if [[ ! "$line" =~ ^([^:[:space:]]+):([a-z0-9-]+):([0-9]+)$ ]]; then
    echo "FAIL: not a <path>:<rule-id>:<line> fingerprint: $line" >&2
    fail=1
    continue
  fi
  path="${BASH_REMATCH[1]}" rule="${BASH_REMATCH[2]}" lineno="${BASH_REMATCH[3]}"
  if [[ "$path" != */tests/* ]]; then
    echo "FAIL: $path is outside a tests/ tree — .gitleaksignore may only exempt test fixtures" >&2
    fail=1
    continue
  fi
  if ! git ls-files --error-unmatch -- "$path" >/dev/null 2>&1; then
    echo "FAIL: $path is not a tracked file (stale fingerprint)" >&2
    fail=1
    continue
  fi
  total=$(wc -l <"$path")
  # A final line without a trailing newline is not counted by wc -l.
  [[ -n "$(tail -c1 "$path")" ]] && total=$((total + 1))
  if (( lineno < 1 || lineno > total )); then
    echo "FAIL: $path has $total lines, fingerprint names line $lineno ($rule) — the flagged line moved" >&2
    fail=1
    continue
  fi
  echo "ok: $path:$lineno ($rule) is a tracked test fixture line"
done <"$IGNORE"

if (( entries == 0 )); then
  echo "FAIL: $IGNORE lists no fingerprints" >&2
  fail=1
fi

# --- every check above must be able to fail: replay against mutated copies ---
if [[ -z "${GITLEAKS_IGNORE_FILE:-}" ]]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  first=$(grep -vE '^(#|$)' "$IGNORE" | head -1)
  path="${first%%:*}"
  mutations=(
    "s#^${path}:#${path}.missing:#"
    "s#^\(${path}:[a-z0-9-]*\):[0-9]*\$#\1:999999#"
    "s#^${path}:#crates/shared/src/lib.rs:#"
  )
  for m in "${mutations[@]}"; do
    sed -e "$m" "$IGNORE" >"$tmp/ignore"
    if GITLEAKS_IGNORE_FILE="$tmp/ignore" bash "$0" >/dev/null 2>&1; then
      echo "FAIL: the checks still pass after mutation: $m" >&2
      fail=1
    else
      echo "ok: mutation is caught: $m"
    fi
  done
fi

if [[ "$fail" -ne 0 ]]; then
  echo "gitleaks_ignore_test FAILED" >&2
  exit 1
fi

echo "gitleaks_ignore_test passed"
