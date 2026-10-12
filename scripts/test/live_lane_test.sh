#!/usr/bin/env bash
# live_lane_test.sh — static checks that the live lane cannot report a pass it
# did not earn (#103 AC-7 / S7).
#
# - live.yml / live-ghes.yml gate on a JOB-level boolean (a step-level env is
#   invisible to the step's own `if:`, which made the lanes skip and still
#   conclude success), keep the secrets scoped to the test step, and fail with
#   an UNVERIFIED error when the secrets are absent.
# - No live test skips by returning early: the shared harness gate panics.
# - live_e2e.rs carries no inline workflow YAML; every pushed workflow is a
#   committed fixture, and the Docker / input fixtures assert real effects.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

fail=0
check() { # check <desc> <command...>
  local desc="$1" out
  shift
  if out=$("$@" 2>&1); then
    echo "ok: $desc"
  else
    echo "FAIL: $desc" >&2
    [[ -n "$out" ]] && printf '%s\n' "$out" >&2
    fail=1
  fi
}
absent() { ! grep -Eq -- "$1" "$2"; }

TESTS=crates/toolu-runner/tests
FIX=$TESTS/fixtures

for pair in live.yml:HAVE_LIVE_TOKEN live-ghes.yml:HAVE_GHES_SECRETS; do
  name=${pair%%:*} flag=${pair#*:}
  wf=.github/workflows/$name
  check "$name keeps only its $flag boolean at job level" \
    grep -Eq "^      $flag: \\\$\\{\\{ secrets\\." "$wf"
  check "$name's guard step reads $flag" grep -Eq "if: \\\$\\{\\{ env\.$flag != 'true' \\}\\}" "$wf"
  check "$name never exposes a secret at job level" \
    absent '^      TOOLU_RUNNER_LIVE[A-Z_]*: \$\{\{ secrets\.' "$wf"
  check "$name fails UNVERIFIED without its secrets" \
    grep -Eq '::error::UNVERIFIED: TOOLU_RUNNER_LIVE' "$wf"
  check "$name has no success path for missing secrets" absent 'skipping .*live tests' "$wf"
  check "$name runs the live tests unconditionally once credentials exist" \
    grep -Eq '^        run: cargo test -p toolu-runner --features live(-ghes)? -- --ignored' "$wf"
done

for f in "$TESTS"/live_e2e.rs "$TESTS"/live_login_e2e.rs "$TESTS"/cache_live_e2e.rs; do
  check "$(basename "$f") has no skip macro" absent 'macro_rules! require_live_env' "$f"
done
check "the shared live gate fails as UNVERIFIED" \
  grep -Eq '"UNVERIFIED: \{var\} is not set' "$TESTS/helpers/live_harness.rs"

check "live_e2e.rs has no inline workflow YAML" absent 'r#"' "$TESTS/live_e2e.rs"
for fixture in $(grep -Eo 'include_str!\("fixtures/[^"]+"\)' "$TESTS/live_e2e.rs" | sed -E 's/.*"(fixtures\/[^"]+)".*/\1/'); do
  check "fixture $fixture exists" test -f "$TESTS/$fixture"
done
check "multistep fixture runs a real docker:// action" \
  grep -Eq '^        uses: docker://alpine@sha256:[0-9a-f]{64}$' "$FIX/multistep-workflow.yml"
check "multistep fixture asserts the container output" \
  grep -Fq "test '\${{ steps.container.outputs.release }}' = 3.20.10" "$FIX/multistep-workflow.yml"
check "multistep fixture asserts the container marker bytes" \
  grep -Fq 'test "$(cat docker-live.txt)" = 3.20.10' "$FIX/multistep-workflow.yml"
check "expression fixture asserts the exact who input" \
  grep -Fq 'test "$WHO" = world' "$FIX/expression-workflow.yml"
check "expression fixture asserts typed inputs" \
  grep -Fq ".who == \"world\" and .count == \"3\" and .enabled == false" "$FIX/expression-workflow.yml"
check "expression test dispatches the empty-input negative" \
  grep -Fq '(serde_json::json!({"who": ""}), "failure")' "$TESTS/live_e2e.rs"

if [[ $fail -ne 0 ]]; then
  echo "live_lane_test FAILED" >&2
  exit 1
fi
echo "live_lane_test passed"
