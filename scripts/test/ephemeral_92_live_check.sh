#!/usr/bin/env bash
# Re-check the #92 live evidence (crates/toolu-runner/tests/fixtures/ephemeral_92_live.json)
# against GitHub: every recorded ephemeral job's workflow run has the recorded
# conclusion, and every runner the lane registered is gone (404).
#
# Needs an authenticated `gh` and `jq`; without them it exits 2 and reports the
# evidence as unverified, never as passing. Not part of the default gate.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
evidence="$root/crates/toolu-runner/tests/fixtures/ephemeral_92_live.json"
repo="$(jq -r '.provenance.repository' "$evidence")"

for tool in gh jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "UNVERIFIED: '$tool' is not installed" >&2
    exit 2
  fi
done
if ! gh api user --jq .login >/dev/null 2>&1; then
  echo "UNVERIFIED: gh is not authenticated" >&2
  exit 2
fi

fail=0
check() {
  if [ "$2" = "$3" ]; then
    echo "ok   $1 ($2)"
  else
    echo "FAIL $1: expected '$3', got '$2'" >&2
    fail=1
  fi
}

while IFS=$'\t' read -r run_id want exit_code; do
  got="$(gh api "repos/$repo/actions/runs/$run_id" --jq .conclusion)"
  check "run $run_id conclusion" "$got" "$want"
  check "run $run_id toolu exit" "$exit_code" "0"
done < <(jq -r '.ephemeral_jobs[] | [.run_id, .run_conclusion, .toolu_exit] | @tsv' "$evidence")

runner_ids="$(jq -r '[.label_registrations[].runner_id, .ephemeral_jobs[].runner_id, .deleted_before_job.runner_id] | .[]' "$evidence")"
for id in $runner_ids; do
  # `gh api -i` prints the status line even for the 404 it exits non-zero on.
  status="$(gh api "repos/$repo/actions/runners/$id" -i 2>&1 | head -1 | awk '{print $2}' || true)"
  check "runner $id removed" "$status" "404"
done

while IFS=$'\t' read -r id want_labels; do
  persisted="$(jq -r --argjson id "$id" '.label_registrations[] | select(.runner_id == $id) | .persisted_labels | map(ascii_downcase) | sort | join(",")' "$evidence")"
  check "runner $id persisted labels match GitHub's (case-insensitive)" "$persisted" "$want_labels"
done < <(jq -r '.label_registrations[] | [.runner_id, (.github_labels | map(ascii_downcase) | sort | join(","))] | @tsv' "$evidence")

if [ "$fail" -ne 0 ]; then
  echo "ephemeral #92 live evidence: FAILED" >&2
  exit 1
fi
echo "ephemeral #92 live evidence: verified"
