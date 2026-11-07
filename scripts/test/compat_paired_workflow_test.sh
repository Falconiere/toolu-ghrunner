#!/usr/bin/env bash
# compat_paired_workflow_test.sh — static validation of
# .github/workflows/compat-paired.yml (#236).
#
# The workflow cannot run before it is on main and it needs a secret that may not
# exist, so its safety properties are pinned from outside: dispatch only, the three
# VMs, the secret guard, per-run labels, verified archives, unregister on every
# exit, and the job names reference_compare.py pairs on.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

# COMPAT_PAIRED_WORKFLOW_FILE lets the negative checks point at a mutated copy.
WORKFLOWS=.github/workflows
WF="${COMPAT_PAIRED_WORKFLOW_FILE:-$WORKFLOWS/compat-paired.yml}"
HOST=scripts/test/compat_paired_host.sh
PROBE="$WORKFLOWS/compat-reference-103-probe.yml"
fail=0

if [[ ! -f "$WF" ]]; then
  echo "FAIL: $WF not found" >&2
  exit 1
fi

want() {
  local desc="$1" pat="$2" file="${3:-$WF}"
  if grep -Eq -- "$pat" "$file"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — pattern not found in $file: $pat" >&2
    fail=1
  fi
}

reject() {
  local desc="$1" pat="$2"
  if grep -Eq -- "$pat" "$WF"; then
    echo "FAIL: $desc — forbidden pattern in $WF: $pat" >&2
    fail=1
  else
    echo "ok: $desc"
  fi
}

# --- triggers: dispatch only ---
want "stays dispatchable" '^  workflow_dispatch:'
for trigger in pull_request pull_request_target push schedule workflow_run; do
  reject "has no $trigger trigger" "^  $trigger:"
done

# --- the three VMs, each with its pinned official archive ---
for vm in ubuntu-latest ubuntu-24.04-arm macos-15; do
  want "a host runs on $vm" "^            vm: $vm$"
done
for archive in linux-x64 linux-arm64 osx-arm64; do
  want "pins the $archive archive" "^            archive: actions-runner-$archive-2\.338\.2\.tar\.gz$"
done
digests=$(grep -Ec '^            sha256: [0-9a-f]{64}$' "$WF")
if [[ "$digests" == 3 ]]; then
  echo "ok: pins three archive digests"
else
  echo "FAIL: expected 3 archive digests, found $digests" >&2
  fail=1
fi
want "checks the archive digest" 'shasum -a 256 --check --strict'
want "downloads v2.338.2 from upstream" 'github\.com/actions/runner/releases/download/v2\.338\.2/'

# --- the secret guard fails as UNVERIFIED, and nothing queues behind it ---
want "guard job exists" '^  guard:'
want "guard keeps only a boolean at job level" "^      HAVE_LIVE_TOKEN: \\$\\{\\{ secrets\\.TOOLU_RUNNER_LIVE_TOKEN != '' \\}\\}$"
want "guard step reads the boolean" "if: \\$\\{\\{ env\\.HAVE_LIVE_TOKEN != 'true' \\}\\}"
want "guard fails UNVERIFIED" '::error::UNVERIFIED: TOOLU_RUNNER_LIVE_TOKEN'
reject "never exposes the secret at job level" '^      (TOOLU_RUNNER_[A-Z_]*|GH_TOKEN): \$\{\{ secrets\.'
needs_guard=$(grep -Ec '^    needs: guard$' "$WF")
if [[ "$needs_guard" == 4 ]]; then
  echo "ok: host, probe, negative and accelerated all need the guard"
else
  echo "FAIL: expected 4 jobs needing the guard, found $needs_guard" >&2
  fail=1
fi

want "guard proves the credential, not just its presence" '::error::credential check failed: HTTP \$status'
want "the credential check names runner administration" '"actions/runners\?per_page=1"'
want "the credential check names job reads" '"actions/runs/\$GITHUB_RUN_ID/jobs\?per_page=1"'

# --- #237: accelerated lane and the hosted API observation ---
accel=$(grep -Ec '^            accelerated: true$' "$WF")
if [[ "$accel" == 1 ]]; then
  echo "ok: exactly one host serves the accelerated lane"
else
  echo "FAIL: expected 1 accelerated host, found $accel" >&2
  fail=1
fi
want "the host learns about the accelerated lane" '^      ACCELERATED_LANE: \$\{\{ matrix\.accelerated \}\}$'
want "accelerated caller is named so the comparator ignores it" '^    name: probe-Linux-X64-toolu-accelerated$'
want "accelerated caller uses a per-run label" '^      runner_label: compat-paired-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-Linux-X64-toolu-accelerated$'
want "surfaces waits for every caller" '^    needs: \[guard, probe, negative, accelerated\]$'
want "surfaces runs once the guard passed" "^    if: \\$\\{\\{ always\(\) && needs\.guard\.result == 'success' \\}\\}$"
want "surfaces reads check runs" '^      checks: read$'
want "surfaces reads deployments" '^      deployments: read$'
want "surfaces runs the checker" 'python3 scripts/test/compat_surfaces_check.py --run'
want "surfaces uploads its report" 'name: compat-237-surfaces$'
for job in annotations secrets deploy container; do
  want "the probe has the $job job" "^  $job:$" "$PROBE"
done
want "the container job pins its image" '^    container: ubuntu@sha256:[0-9a-f]{64}$' "$PROBE"
want "the consumer waits for the #237 producers" '^    needs: \[contexts, docker, services, secrets, container\]$' "$PROBE"
want "the secret URL comes from a masked step output" '^      url: \$\{\{ steps\.emit\.outputs\.url \}\}$' "$PROBE"

# --- per-run labels: no run can pick up another run's jobs ---
want "host labels are per run" '^      LABEL_PREFIX: compat-paired-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-\$\{\{ matrix\.platform \}\}$'
callers=$(grep -Ec '^      runner_label: compat-paired-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-\$\{\{ matrix\.platform \}\}-\$\{\{ matrix\.lane \}\}$' "$WF")
if [[ "$callers" == 2 ]]; then
  echo "ok: both probe callers use per-run labels"
else
  echo "FAIL: expected 2 per-run runner_label lines, found $callers" >&2
  fail=1
fi
reject "no runner label is fixed" 'runner_label: toolu-103-'

# --- unregister on every exit ---
if grep -E -A1 '^      - name: Unregister both lanes$' "$WF" | grep -Eq '^        if: always\(\)$'; then
  echo "ok: unregister runs under if: always()"
else
  echo "FAIL: the unregister step must carry if: always()" >&2
  fail=1
fi
want "unregister calls the host script" "bash $HOST unregister"
want "a dead host cannot leave probes queued" "needs\\.host\\.result != 'success'"

# --- names and artifacts reference_compare.py reads ---
want "probe caller names carry the platform" '^    name: probe-\$\{\{ matrix\.platform \}\}-\$\{\{ matrix\.lane \}\}$'
want "negative caller names carry the platform" '^    name: negative-\$\{\{ matrix\.platform \}\}-\$\{\{ matrix\.lane \}\}$'
want "probe artifacts stay unique per platform" '^      lane: \$\{\{ matrix\.platform \}\}-\$\{\{ matrix\.lane \}\}$'
want "the VM's containers are uploaded" 'name: compat-paired-\$\{\{ matrix\.platform \}\}-containers'
want "macOS skips the Docker job" "run_docker: \\$\\{\\{ matrix\\.platform != 'macOS-ARM64' \\}\\}"

# --- the probe keeps its Docker gate and its default ---
want "the probe gates its docker job" '^    if: inputs\.run_docker$' "$PROBE"
want "the probe defaults to running Docker" '^        default: true$' "$PROBE"

# --- the host script ---
if bash -n "$HOST"; then
  echo "ok: $HOST parses"
else
  echo "FAIL: $HOST does not parse" >&2
  fail=1
fi
want "lanes do not inherit the VM's job env" 'clean_env=\(env -i ' "$HOST"
want "the JIT config is masked" '::add-mask::\$jit' "$HOST"

# --- negative checks: the assertions above must be able to fail ---
if [[ -z "${COMPAT_PAIRED_WORKFLOW_FILE:-}" ]]; then
  # The host script's control flow, with GitHub and both runners stood in for:
  # nothing else can exercise it before the first dispatch.
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/tools" "$tmp/official/bin" "$tmp/official-dies"
  cat >"$tmp/tools/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
  *generate-jitconfig*) [[ -z "${FAKE_NO_JIT:-}" ]] && echo fake-jit ;;
  *--method\ DELETE*)
    [[ -n "${FAKE_DELETE_WORKS:-}" ]] || exit 1
    : >"$FAKE_RUNNERS"
    ;;
  # Jobs finish only once the listener is up, so stopping the lanes has one to stop.
  */jobs*) [[ -f "$OFFICIAL_DIR/listening" ]] && cat "$FAKE_JOBS" ;;
  */runners*) cat "$FAKE_RUNNERS" ;;
esac
GH
  printf '#!/usr/bin/env bash\n"$PWD/bin/Runner.Listener"\n' >"$tmp/official/run.sh"
  printf '#!/usr/bin/env bash\ntouch "$PWD/listening"\nwhile :; do sleep 1; done\n' >"$tmp/official/bin/Runner.Listener"
  cat >"$tmp/tools/toolu" <<'TOOLU'
#!/usr/bin/env bash
[[ "$1" == run ]] && while :; do sleep 1; done
exit 0
TOOLU
  # The accelerated lane's container: `run` records its arguments and whether the
  # token arrived through the environment; FAKE_DOCKER_DEAD stops it.
  cat >"$tmp/tools/docker" <<'DOCKER'
#!/usr/bin/env bash
case "$1" in
  run)
    printf '%s\n' "$@" >"$FAKE_DOCKER_RUN"
    [[ "${TOOLU_RUNNER_TOKEN:-}" == fake ]] && echo token-in-env >>"$FAKE_DOCKER_RUN"
    echo container-id
    ;;
  inspect) [[ -n "${FAKE_DOCKER_DEAD:-}" ]] && echo false || echo true ;;
  logs) echo "accelerated runner output" ;;
esac
exit 0
DOCKER
  # The lanes run under `env -i`, so a dying toolu is a binary of its own.
  printf '#!/usr/bin/env bash\n[[ "$1" == run ]] && exit 2\nexit 0\n' >"$tmp/tools/toolu-dies"
  printf '#!/usr/bin/env bash\nexit 3\n' >"$tmp/official-dies/run.sh"
  chmod +x "$tmp/tools/gh" "$tmp/tools/toolu" "$tmp/tools/docker" "$tmp/tools/toolu-dies" "$tmp/official/run.sh" "$tmp/official-dies/run.sh" \
    "$tmp/official/bin/Runner.Listener"
  for caller in probe-Linux-X64-toolu probe-Linux-X64-official negative-Linux-X64-toolu negative-Linux-X64-official; do
    printf 'completed %s / contexts (alpha, 7, true)\ncompleted %s / consumer\n' "$caller" "$caller"
  done >"$tmp/jobs"
  : >"$tmp/none"
  echo 4242 >"$tmp/survivor"
  host() {
    (
      export FAKE_JOBS="$tmp/jobs" FAKE_NO_JIT="${FAKE_NO_JIT:-}" GH_TOKEN=fake GITHUB_REPOSITORY=o/r GITHUB_RUN_ID=1 RUNNER_TEMP="$tmp"
      export PLATFORM=Linux-X64 LABEL_PREFIX="compat-paired-$GITHUB_RUN_ID-Linux-X64" POLL_SECONDS=0
      export ACCELERATED_LANE="${FAKE_ACCELERATED:-}" FAKE_DOCKER_RUN="$tmp/docker-run"
      export FAKE_DOCKER_DEAD="${FAKE_DOCKER_DEAD:-}"
      export OFFICIAL_DIR="${FAKE_OFFICIAL:-$tmp/official}" TOOLU_BIN="${FAKE_TOOLU:-$tmp/tools/toolu}"
      PATH="$tmp/tools:$PATH" timeout_guard bash "$HOST" "$@"
    )
  }
  timeout_guard() { # Bounded so a lane that never stops fails here, not in CI's timeout.
    "$@" &
    local pid=$! waited=0
    while kill -0 "$pid" 2>/dev/null && ((waited < 300)); do
      sleep 0.1
      waited=$((waited + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid"
      return 124
    fi
    wait "$pid"
  }
  expect() { # expect <desc> <wanted exit> <actual exit>
    if [[ "$2" == "$3" ]]; then
      echo "ok: $1"
    else
      echo "FAIL: $1 — exit $3, wanted $2" >&2
      fail=1
    fi
  }
  FAKE_RUNNERS="$tmp/none" host run >/dev/null 2>&1
  expect "lanes stop once the probes completed" 0 $?
  [[ -f "$tmp/official/listening" ]] && ! pgrep -f "$tmp/official/bin/Runner.Listener" >/dev/null &&
    echo "ok: the official listener does not outlive the lanes" || {
    echo "FAIL: the official listener was not stopped" >&2
    pkill -f "$tmp/official/bin/Runner.Listener"
    fail=1
  }
  # Completed jobs alone are not done: the last consumer may not be listed yet.
  sed -i.bak '$d' "$tmp/jobs"
  FAKE_RUNNERS="$tmp/none" FAKE_TOOLU="$tmp/tools/toolu-dies" host run >/dev/null 2>&1
  expect "a dead toolu lane fails the step while a consumer is unlisted" 1 $?
  FAKE_RUNNERS="$tmp/none" FAKE_NO_JIT=1 host run >/dev/null 2>&1
  expect "a dead official lane fails the step while a consumer is unlisted" 1 $?
  # This lane always mints before it dies, so both checks below see a real JIT.
  out=$(FAKE_RUNNERS="$tmp/none" FAKE_OFFICIAL="$tmp/official-dies" host run 2>&1)
  expect "an official runner that cannot start fails the step" 1 $?
  [[ "$out" == *"::add-mask::fake-jit"* ]] && echo "ok: the JIT config is masked on the step's stdout" || {
    echo "FAIL: the mask command did not reach the step's stdout: $out" >&2
    fail=1
  }
  logged=$(cat "$tmp/official.log" "$tmp/toolu.log" 2>/dev/null)
  [[ "$logged" != *fake-jit* ]] && echo "ok: the JIT config stays out of the lane logs" || {
    echo "FAIL: the JIT config reached a lane log" >&2
    fail=1
  }
  # The accelerated lane adds a fifth consumer: with only the four forwarder and
  # official consumers done, the host keeps polling, so a stopped container fails it.
  for caller in probe-Linux-X64-toolu probe-Linux-X64-official negative-Linux-X64-toolu negative-Linux-X64-official; do
    printf 'completed %s / consumer\n' "$caller"
  done >"$tmp/jobs"
  FAKE_RUNNERS="$tmp/none" FAKE_ACCELERATED=true FAKE_DOCKER_DEAD=1 host run >/dev/null 2>&1
  expect "four consumers do not finish an accelerated host" 1 $?
  printf 'completed probe-Linux-X64-toolu-accelerated / consumer\n' >>"$tmp/jobs"
  out=$(FAKE_RUNNERS="$tmp/none" FAKE_ACCELERATED=true host run 2>&1)
  expect "an accelerated host stops once its five consumers completed" 0 $?
  args=$(cat "$tmp/docker-run")
  [[ "$args" == *ubuntu@sha256:* && "$args" == *$'\n-e\nTOOLU_RUNNER_TOKEN\n'* && "$args" == *token-in-env* &&
    "$args" != *TOOLU_RUNNER_TOKEN=* ]] &&
    echo "ok: the accelerated runner runs in a pinned container and gets the token by name only" || {
    echo "FAIL: accelerated container arguments wrong: $args" >&2
    fail=1
  }
  # The rewrite itself, then its check, then the run: each must be present in order.
  [[ "$args" == *'sed -i "s/^mode = \"forwarder\"\$/mode = \"accelerated\"/"'*'grep -qx "mode = \"accelerated\""'*"exec toolu-runner run"* ]] &&
    echo "ok: the containerized lane switches [services] mode before it runs" || {
    echo "FAIL: accelerated mode switch missing: $args" >&2
    fail=1
  }
  [[ "$out" == *"toolu-accelerated lane log"*"accelerated runner output"* ]] &&
    echo "ok: the accelerated lane log is printed" || {
    echo "FAIL: accelerated lane log missing: $out" >&2
    fail=1
  }

  FAKE_RUNNERS="$tmp/none" host unregister >/dev/null 2>&1
  expect "unregister passes with no runner left" 0 $?
  cp "$tmp/survivor" "$tmp/deletable"
  FAKE_RUNNERS="$tmp/deletable" FAKE_DELETE_WORKS=1 UNREGISTER_ATTEMPTS=1 host unregister >/dev/null 2>&1
  expect "unregister passes when the last attempt's delete worked" 0 $?
  out=$(FAKE_RUNNERS="$tmp/survivor" host unregister 2>&1)
  expect "unregister fails while a runner survives" 1 $?
  [[ "$out" == *"::error::runners still registered"*4242* ]] && echo "ok: the survivor is named" || {
    echo "FAIL: survivor not named: $out" >&2
    fail=1
  }

  mutate() {
    local desc="$1" sedexpr="$2" copy
    copy=$(mktemp)
    sed -E "$sedexpr" "$WF" >"$copy"
    if cmp -s "$WF" "$copy"; then
      echo "FAIL: mutation '$desc' changed nothing" >&2
      fail=1
    elif COMPAT_PAIRED_WORKFLOW_FILE="$copy" bash "$0" >/dev/null 2>&1; then
      echo "FAIL: mutation '$desc' was not detected" >&2
      fail=1
    else
      echo "ok: detects $desc"
    fi
    rm -f "$copy"
  }
  mutate "a pull_request trigger" 's/^  workflow_dispatch:/  workflow_dispatch:\n  pull_request:/'
  mutate "a dropped VM" '/vm: ubuntu-24.04-arm/d'
  mutate "a dropped digest" '/sha256: 670adb/d'
  mutate "a skipped digest check" '/shasum -a 256/d'
  mutate "a removed secret guard" '/UNVERIFIED/d'
  mutate "a presence-only credential check" '/credential check failed/d'
  mutate "a surfaces job without check reads" '/^      checks: read$/d'
  mutate "a second accelerated host" 's/^            sha256: 670adb.*/&\n            accelerated: true/'
  mutate "a fixed host label" 's/^      LABEL_PREFIX: .*/      LABEL_PREFIX: compat-paired/'
  mutate "a fixed probe label" 's/^      runner_label: .*/      runner_label: toolu-103-toolu/'
  mutate "an unregister that can be skipped" '/- name: Unregister both lanes/{n;s/if: always\(\)/if: success()/;}'
  mutate "a platform-less caller name" 's/^    name: probe-.*/    name: probe-${{ matrix.lane }}/'
fi

if [[ "$fail" -ne 0 ]]; then
  echo "compat_paired_workflow_test FAILED" >&2
  exit 1
fi
echo "compat_paired_workflow_test passed"
