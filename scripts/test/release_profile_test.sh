#!/usr/bin/env bash
# release_profile_test.sh — pins the shipped `[profile.release]` (#112).
#
# The release profile is a measured decision, recorded with its benchmark
# evidence in docs/release-profile.md. This test fails if Cargo.toml drifts
# from that decision (opt-level, lto) or loses a size lever it relies on
# (codegen-units = 1, strip, panic = "abort"), and re-runs every check against
# a mutated copy that must fail — a check that cannot fail proves nothing.
# RELEASE_PROFILE_FILE points the checks at another manifest (the mutation run).
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

EXPECT_OPT='opt-level = "s"'
EXPECT_LTO='lto = "fat"'

# profile_lines FILE — the body of the [profile.release] table, nothing else.
profile_lines() {
  awk '/^\[profile\.release\]/{on=1; next} /^\[/{on=0} on && NF && !/^[[:space:]]*#/' "$1"
}

# run_checks FILE — one "FAIL: ..." line per violated property.
run_checks() {
  local body
  body=$(profile_lines "$1")
  [[ -n "$body" ]] || { echo "FAIL: no [profile.release] table in $1"; return; }
  grep -qxF "$EXPECT_OPT" <<<"$body" || echo "FAIL: release profile is not $EXPECT_OPT"
  grep -qxF "$EXPECT_LTO" <<<"$body" || echo "FAIL: release profile is not $EXPECT_LTO"
  grep -qxF 'codegen-units = 1' <<<"$body" || echo "FAIL: codegen-units = 1 missing"
  grep -qxF 'strip = "symbols"' <<<"$body" || echo "FAIL: strip = \"symbols\" missing"
  grep -qxF 'panic = "abort"' <<<"$body" || echo "FAIL: panic = \"abort\" missing"
}

fail=0
target="${RELEASE_PROFILE_FILE:-Cargo.toml}"
out=$(run_checks "$target")
if [[ -n "$out" ]]; then echo "$out" >&2; fail=1; fi

# Negative runs: each mutation must be caught.
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
mutate() { # mutate DESC SED-EXPR
  sed "$2" Cargo.toml >"$tmp"
  if [[ -z "$(run_checks "$tmp")" ]]; then
    echo "FAIL: mutation not detected: $1" >&2
    fail=1
  fi
}
mutate "opt-level 3" '/^\[profile\.release\]/,/^$/s/^opt-level = "s"$/opt-level = 3/'
mutate "thin lto" '/^\[profile\.release\]/,/^$/s/^lto = "fat"$/lto = "thin"/'
mutate "no strip" '/^\[profile\.release\]/,/^$/s/^strip = "symbols"$/strip = "none"/'
mutate "unwinding panic" '/^\[profile\.release\]/,/^$/s/^panic = "abort"$/panic = "unwind"/'
mutate "more codegen units" '/^\[profile\.release\]/,/^$/s/^codegen-units = 1$/codegen-units = 16/'

if [[ $fail -ne 0 ]]; then
  echo "release_profile_test: FAILED" >&2
  exit 1
fi
echo "release_profile_test: all passed"
