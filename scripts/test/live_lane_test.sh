#!/usr/bin/env bash
# live_lane_test.sh — static checks that the live lane cannot report a pass it
# did not earn (#103 AC-7 / S7).
#
# - live.yml maps its secret at JOB level (a step-level env is invisible to the
#   step's own `if:`, which made the lane skip daily and conclude success) and
#   fails with an UNVERIFIED error when the secret is absent.
# - No live test skips by returning early: the shared harness gate panics.
# - live_e2e.rs carries no inline workflow YAML; every pushed workflow is a
#   committed fixture, and the Docker / input fixtures assert real effects.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

fail=0
check() { # check <desc> <command...>
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc" >&2
    fail=1
  fi
}
absent() { ! grep -Eq -- "$1" "$2"; }

WF=.github/workflows/live.yml
TESTS=crates/toolu-runner/tests
FIX=$TESTS/fixtures

check "live.yml maps the secret at job level" \
  grep -Eq '^    env:$' "$WF"
check "live.yml job env carries the secret" \
  grep -Eq '^      TOOLU_RUNNER_LIVE_TOKEN: \$\{\{ secrets\.TOOLU_RUNNER_LIVE_TOKEN \}\}$' "$WF"
check "live.yml fails UNVERIFIED without the secret" \
  grep -Eq 'UNVERIFIED: TOOLU_RUNNER_LIVE_TOKEN is not set' "$WF"
check "live.yml has no success path for a missing secret" absent 'skipping live tests' "$WF"
check "live tests run unconditionally once credentials exist" \
  grep -Eq '^        run: cargo test -p toolu-runner --features live -- --ignored' "$WF"

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
