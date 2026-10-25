#!/usr/bin/env bash
# ci_workflow_test.sh — static validation of .github/workflows/ci.yml.
#
# The gate is ONE command locally (./tools/check.sh all) and CI is required to
# run every one of its layers. Since the parallel split, those layers live in
# several jobs — `lint`, the sharded `test` / `test-macos` matrices,
# `docker-replay`, `scripts` — behind two aggregator jobs, `ci` and
# `ci-macos`, whose names branch protection requires. This pins what a reader
# cannot verify by running anything:
#
# - every layer of the gate still runs in SOME job, on the right OS;
# - the shards together run the whole suite exactly once: `--partition
#   slice:k/N` with a literal N that equals the matrix length (a larger N would
#   silently skip part of the suite; nextest rejects only k > N), and every
#   once-per-leg step that is gated on a shard names a shard that exists — a
#   step pinned to shard 3 silently never runs again if the matrix shrinks to
#   two;
# - the aggregators need every real job and reject every result but success;
# - the macOS leg exists, runs on a supported arm64 image, and is not inert.
#
# The macOS leg is the regression this file originally existed for.
# `runner_os()` shipped hardcoded to "Linux", the launchd unit shipped without
# a PATH, and the Docker client shipped pinned to /var/run/docker.sock — three
# macOS-only defects that lived in main because nothing ever compiled, let alone
# tested, on a Mac. Deleting the leg would make that blind spot silently return,
# and a deleted job reports nothing rather than failing, so assert it from
# outside.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

# CI_WORKFLOW_FILE lets a negative check point the test at a mutated copy.
WF="${CI_WORKFLOW_FILE:-.github/workflows/ci.yml}"
fail=0

if [[ ! -f "$WF" ]]; then
  echo "FAIL: $WF not found" >&2
  exit 1
fi

# want <desc> <pattern> — assert a pattern is PRESENT in the workflow.
want() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in $WF: $pat" >&2
    fail=1
  fi
}

# job_body <job-key> — everything from `<job-key>:` up to the next job key at the
# same indent (or EOF). Job-scoped extraction is what makes the assertions below
# mean anything: a whole-file grep passes as long as SOME job carries the step,
# which is exactly the drift the legs can suffer.
job_body() {
  awk -v key="  $1:" '
    $0 == key                                  { injob = 1; print; next }
    injob && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { injob = 0 }
    injob                                      { print }
  ' "$WF"
}

# want_in <job-body> <desc> <pattern> — assert a pattern is present in ONE job,
# not merely somewhere in the file. `cargo nextest run` appears in both test
# jobs, so a whole-file grep would pass even if the macOS job had no test step
# — exactly the hole this file exists to close.
want_in() {
  local body="$1" desc="$2" pat="$3"
  if printf '%s\n' "$body" | grep -Eq -- "$pat"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in the job: $pat" >&2
    fail=1
  fi
}

# --- triggers (a leg that never fires proves nothing) ---
want "workflow runs on pull_request"      "^  pull_request:"
want "workflow runs on pushes to main"    "^  push:"

# --- #119: superseded PR runs are cancelled, main never is ---
want "ci groups runs per ref"             '^  group: ci-\$\{\{ github\.ref \}\}$'
want "cancel-in-progress only for pull_request" \
  "^  cancel-in-progress: \\$\\{\\{ github\\.event_name == 'pull_request' \\}\\}$"

# --- the jobs ---
for job in lint test docker-replay scripts ci test-macos ci-macos; do
  want "a '$job' job exists" "^  $job:"
done

LINT_JOB=$(job_body lint)
TEST_JOB=$(job_body test)
DOCKER_JOB=$(job_body docker-replay)
SCRIPTS_JOB=$(job_body scripts)
CI_JOB=$(job_body ci)
MACOS_JOB=$(job_body test-macos)
CI_MACOS_JOB=$(job_body ci-macos)

for pair in "LINT_JOB:lint" "TEST_JOB:test" "DOCKER_JOB:docker-replay" "SCRIPTS_JOB:scripts" "CI_JOB:ci" "MACOS_JOB:test-macos" "CI_MACOS_JOB:ci-macos"; do
  var="${pair%%:*}"
  if [[ -z "${!var}" ]]; then
    echo "FAIL: could not extract the ${pair##*:} job from $WF" >&2
    exit 1
  fi
done

want_lint() { want_in "$LINT_JOB" "$1" "$2"; }
want_test() { want_in "$TEST_JOB" "$1" "$2"; }
want_docker() { want_in "$DOCKER_JOB" "$1" "$2"; }
want_scripts() { want_in "$SCRIPTS_JOB" "$1" "$2"; }
want_macos() { want_in "$MACOS_JOB" "$1" "$2"; }

# --- lint: the OS-independent layers of ./tools/check.sh all ---
want_lint "lint checks formatting"            "cargo fmt --all -- --check"
want_lint "lint runs clippy as an error gate" "cargo clippy --workspace --all-targets -- -D warnings"
want_lint "lint lints the capture feature build" \
  "cargo clippy -p toolu-runner --features capture --all-targets -- -D warnings"
want_lint "lint enforces the #[allow] ban"    "\./tools/check\.sh no-allow"
want_lint "lint runs the guardrails"          "bash scripts/guardrails/run\.sh"
want_lint "lint keeps protocol I/O-free"      "cargo tree -p protocol"

# --- #110: prebuilt, pinned ast-grep ---
# Building ast-grep from source cost 76-117 s per run. The install must stay a
# version + SHA-256 pinned download, placed before the Structure step that needs
# it, and must never fall back to compiling it.
want_lint "lint pins the ast-grep version"      "AST_GREP_VERSION: \"[0-9]+\.[0-9]+\.[0-9]+\""
want_lint "lint pins the ast-grep SHA-256"      "AST_GREP_SHA256: [0-9a-f]{64}"
want_lint "lint verifies the ast-grep checksum" "sha256sum --check --strict"
if printf '%s\n' "$LINT_JOB" | grep -Eq -- "cargo install ast-grep"; then
  echo "FAIL: the lint job compiles ast-grep from source — use the pinned prebuilt binary" >&2
  fail=1
else
  echo "ok: lint does not cargo-install ast-grep"
fi
if printf '%s\n' "$LINT_JOB" | awk '
    /name: Install ast-grep/ { inst = NR }
    /name: Structure/        { st = NR }
    END { exit !(inst && st && inst < st) }'; then
  echo "ok: ast-grep is installed before the Structure step"
else
  echo "FAIL: 'Install ast-grep' must precede 'Structure' in the lint job" >&2
  fail=1
fi

# --- test / test-macos: the sharded nextest run (#118 nextest, sharded) ---
PARTITION='cargo nextest run --workspace --partition slice:\$\{\{ matrix\.shard \}\}/[0-9]+$'
for leg in test test-macos; do
  if [[ "$leg" == test ]]; then body="$TEST_JOB"; else body="$MACOS_JOB"; fi
  want_in "$body" "$leg installs cargo-nextest"                 "tool: cargo-nextest"
  want_in "$body" "$leg runs its slice of the suite with nextest" "$PARTITION"
  want_in "$body" "$leg runs the doctests nextest skips"         "cargo test --workspace --doc"
  want_in "$body" "$leg shards over a matrix"                    "^        shard: \[[0-9]+(, [0-9]+)+\]$"
  want_in "$body" "$leg does not stop the other shards on a failure" "fail-fast: false"
done
# The ubuntu leg's once-per-leg scripts: install.sh under real login shells.
want_test "ubuntu runs the install.sh smoke tests"       "bash tests/install_sh_test\.sh"
want_test "ubuntu runs the portable install.sh test"     "bash tests/install_sh_bash32_test\.sh"
want_test "ubuntu installs zsh and fish for the install.sh login-shell tests" \
  "apt-get install -y --no-install-recommends zsh fish"

# --- test-macos: the OS-dependent half on a supported arm64 image ---
# macos-14 is retired on 2026-11-02 (actions/runner-images#13518); macos-13 and
# the -intel images are x86_64, and arm64 is the deployment target this leg
# exists to cover.
want_macos "the macOS job is a supported arm64 image"  "^ +runs-on: macos-(1[5-9]|[2-9][0-9])$"
want_macos "macOS runs clippy as an error gate" "cargo clippy --workspace --all-targets -- -D warnings"
want_macos "macOS runs install.sh under /bin/bash 3.2" "INSTALL_SH_REQUIRE_BASH3: \"1\""
want_macos "macOS runs the bash 3.2 install.sh test"   "bash tests/install_sh_bash32_test\.sh"
# plutil only exists on macOS — validating the generated LaunchAgent as a real
# plist is the single thing this leg can do that ubuntu cannot.
want_macos "macOS validates the launchd plist"  "scripts/test/plist_test\.sh"

# --- docker-replay: the Linux Docker lane (#103 `ci-docker` evidence) ---
want_docker "the Docker lane keeps its step name (compat_matrix_check.py parses it)" \
  "name: Docker action runtime and production replay"
want_docker "the Docker lane replays the #103 docker capture" \
  "--test integration -- compat_probe_replay_test:: --ignored"

# --- scripts: the shell and Python checks ---
want_scripts "scripts runs the release-script tests"      'bash "scripts/test/\$\{t\}_test\.sh"'
want_scripts "scripts runs the check.sh test"             "check_script"
want_scripts "scripts runs this test"                     "ci_workflow"
want_scripts "scripts runs the shell-templates workflow test" "shell_templates_workflow"
want_scripts "scripts runs the live-lane honesty test"    "live_lane"
want_scripts "scripts checks the #103 compatibility matrix" "compat_matrix_check\.py --check"
want_scripts "scripts re-verifies the #103 reference report" \
  "reference_compare\.py --verify docs/compat-reference-103\.json"
want_scripts "scripts checks the #88 CompleteJob evidence" "completejob_evidence_check\.py --local"

# --- Rust build cache: after the toolchain, saved from main only ---
# The cache step must come after the toolchain: rust-cache keys on rustc -vV.
# `save-if` main-only: a per-PR copy of main's key is what pushed the repo over
# the 10 GB cache limit.
for leg in lint test docker-replay test-macos; do
  body=$(job_body "$leg")
  if printf '%s\n' "$body" | awk '
      /dtolnay\/rust-toolchain@/ { tc = 1 }
      /Swatinem\/rust-cache@v[0-9]+/ { if (tc) ok = 1 }
      END { exit !ok }'; then
    echo "ok: $leg restores a Rust cache after the toolchain"
  else
    echo "FAIL: $leg needs a Swatinem/rust-cache step after dtolnay/rust-toolchain" >&2
    fail=1
  fi
  want_in "$body" "$leg saves the Rust cache from main only" \
    "^          save-if: \\$\\{\\{ github\\.ref == 'refs/heads/main' \\}\\}$"
done

# --- aggregators: the required contexts ---
for agg in ci ci-macos; do
  body=$(job_body "$agg")
  want_in "$body" "$agg runs even when a dependency failed" "^    if: always\(\)$"
  want_in "$body" "$agg reads every dependency's result"     'join\(needs\.\*\.result'
  want_in "$body" "$agg rejects every result but success"    '== success \]\] \|\| exit 1'
done
want_in "$CI_JOB" "ci needs the Linux jobs" "^    needs: \[lint, test, docker-replay, scripts\]$"
want_in "$CI_MACOS_JOB" "ci-macos needs the macOS shards" "^    needs: \[test-macos\]$"

# --- no job may be inert ---
# `continue-on-error` file-wide (no job may carry it); `if: false` on any job.
if grep -Eq -- "^ +continue-on-error: true" "$WF"; then
  echo "FAIL: no CI job may be continue-on-error — it reports green while failing" >&2
  fail=1
else
  echo "ok: no job is continue-on-error"
fi
if grep -Eq -- "^    if:[[:space:]]*(false|\\\$\\{\\{[[:space:]]*false[[:space:]]*\\}\\})[[:space:]]*$" "$WF"; then
  echo "FAIL: a job is gated off with 'if: false' — it never runs" >&2
  fail=1
else
  echo "ok: no job is gated off"
fi

# --- optional: deep parse if PyYAML is available ---
# The grep tier pins the strings; this tier pins the shape: the exact job set,
# a timeout on every job, every real job behind exactly one aggregator, and
# every shard-gated step naming a shard that exists.
if python3 -c 'import yaml' >/dev/null 2>&1; then
  if python3 - "$WF" <<'PY'
import re, sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
jobs = wf["jobs"]
aggregators = {"ci", "ci-macos"}
real = {"lint", "test", "docker-replay", "scripts", "test-macos"}
assert set(jobs) == real | aggregators, f"jobs: {sorted(jobs)}"
for name, job in jobs.items():
    assert isinstance(job.get("timeout-minutes"), int), f"{name}: no timeout-minutes"
covered = set()
for agg in aggregators:
    needs = jobs[agg]["needs"]
    assert jobs[agg].get("if") == "always()", f"{agg}: if={jobs[agg].get('if')!r}"
    assert not (set(needs) & covered), f"{agg}: a job is aggregated twice"
    covered |= set(needs)
assert covered == real, f"jobs behind no aggregator: {sorted(real - covered)}"
# Shard-gated steps: `if: matrix.shard == N` must name a shard in the matrix,
# and the steps that must run once per leg must be gated (not run N times,
# not dropped).
once_per_leg = {
    "test": {"Doctests", "Install zsh and fish for the install.sh tests", "install.sh login-shell tests"},
    "test-macos": {"Clippy", "Doctests", "install.sh under macOS /bin/bash 3.2", "Validate the launchd plist"},
}
for leg, names in once_per_leg.items():
    shards = jobs[leg]["strategy"]["matrix"]["shard"]
    assert len(shards) >= 2 and shards == list(range(1, len(shards) + 1)), f"{leg}: shards {shards}"
    seen = set()
    for step in jobs[leg]["steps"]:
        cond = step.get("if")
        if cond is None:
            continue
        m = re.fullmatch(r"matrix\.shard == (\d+)", str(cond))
        assert m, f"{leg}: step {step.get('name')!r} has an unexpected condition {cond!r}"
        assert int(m.group(1)) in shards, f"{leg}: step {step.get('name')!r} is pinned to shard {m.group(1)}, matrix has {shards}"
        seen.add(step["name"])
    assert seen == names, f"{leg}: shard-gated steps {sorted(seen)} != expected {sorted(names)}"
    tests = [s for s in jobs[leg]["steps"] if s.get("name") == "Test"]
    assert len(tests) == 1 and "if" not in tests[0], f"{leg}: the Test step must run on every shard"
    # The literal denominator must equal the matrix length: with fewer shards
    # than the denominator, the missing slices are never run and nothing fails.
    m = re.search(r"--partition slice:\$\{\{ matrix\.shard \}\}/(\d+)$", tests[0]["run"].strip())
    assert m, f"{leg}: the Test step must run `--partition slice:${{{{ matrix.shard }}}}/<N>`, got {tests[0]['run']!r}"
    assert int(m.group(1)) == len(shards), f"{leg}: partition denominator {m.group(1)} != {len(shards)} shards in the matrix"
print("ok: PyYAML deep-check (job set + timeouts + aggregator coverage + shard-gated steps name real shards + denominators match the matrices)")
PY
  then :; else
    echo "FAIL: PyYAML deep-check failed" >&2
    fail=1
  fi
else
  echo "# PyYAML unavailable — skipped deep parse (grep tier covers invariants)"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "ci_workflow_test FAILED" >&2
  exit 1
fi

echo "ci_workflow_test passed"
