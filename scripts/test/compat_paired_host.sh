#!/usr/bin/env bash
# compat_paired_host.sh — one VM's side of .github/workflows/compat-paired.yml.
#
#   run         register both lanes under per-run labels and keep them serving
#               until this platform's probe jobs have all completed
#   unregister  delete every runner carrying this platform's per-run labels
#   report      write $RUNNER_TEMP/host-251.json (#251: counts and booleans only)
#   capture     sanitize the toolu lane's `stack` probe message into $2 (#251)
#
# Environment: GH_TOKEN (administration:write), GITHUB_REPOSITORY, GITHUB_RUN_ID,
# RUNNER_TEMP, PLATFORM (Linux-X64, Linux-ARM64, macOS-ARM64), LABEL_PREFIX, and
# for `run`: OFFICIAL_DIR, TOOLU_BIN. ACCELERATED_LANE=true adds a third lane (#237):
# a second toolu registration with `[services] mode = "accelerated"` whose runner
# process runs inside an ubuntu container, so every job it takes (artifact
# producer and consumer alike) executes in that container and reaches the local
# accelerated proxy in its own network namespace. GitHub job containers
# (`container:`) stay refused in accelerated mode; this is not that. Each lane logs to a file under
# RUNNER_TEMP, printed on exit, so a lane still alive cannot hold the step's stdout.
# EXTENDED_LANES=true (#251) adds, per VM: two more official runner directories
# and two more toolu registrations (sharing one --work, so the stable-workspace
# lease must keep concurrent jobs apart) serving `<lane>-pool`; a toolu registration
# with `[workspace] mode = "fresh"`; and an ephemeral toolu registration.
set -euo pipefail

POLL_SECONDS="${POLL_SECONDS:-15}"
ACCELERATED_IMAGE=ubuntu@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852
ACCELERATED_CONTAINER="toolu-accelerated-${GITHUB_RUN_ID:-0}"

# One `consumer` per variant and lane. It needs every other probe job, so it ends last.
# The accelerated lane runs the probe variant only.
CONSUMER_JOBS=4
[[ "${ACCELERATED_LANE:-}" == true ]] && CONSUMER_JOBS=5
# The toolu-only lifecycle jobs (#251): fresh-1, fresh-2, ephemeral.
LIFECYCLE_JOBS=0
[[ "${EXTENDED_LANES:-}" == true ]] && LIFECYCLE_JOBS=3
UNREGISTER_ATTEMPTS="${UNREGISTER_ATTEMPTS:-6}"
RUNNERS_API="repos/$GITHUB_REPOSITORY/actions/runners"

# The VM's own job variables (GITHUB_*, RUNNER_*, ACTIONS_*) must not reach
# either lane's steps, or they would hide or fake a difference.
clean_env=(env -i "HOME=$HOME" "PATH=$PATH" "USER=$(id -un)" "LANG=${LANG:-C.UTF-8}" "TMPDIR=${TMPDIR:-/tmp}")

# A JIT runner takes exactly one job, so the official lane mints one per job.
# toolu's `run` does the same on its own. Fd 3 is the step's stdout: the mask
# command must reach it without the credential landing in the lane's log file.
official_lane() {
  local dir="${1:-$OFFICIAL_DIR}" label="${2:-$LABEL_PREFIX-official}" name="${3:-$LABEL_PREFIX-official}"
  local serial=0 jit
  while :; do
    serial=$((serial + 1))
    jit=$(gh api --method POST "$RUNNERS_API/generate-jitconfig" \
      -f "name=$name-$serial" -F runner_group_id=1 -f work_folder=_work \
      -f 'labels[]=self-hosted' -f "labels[]=${PLATFORM%-*}" -f "labels[]=${PLATFORM#*-}" \
      -f "labels[]=$label" --jq .encoded_jit_config)
    echo "::add-mask::$jit" >&3
    # A job's own failure still exits 0, so non-zero means the runner is broken.
    (cd "$dir" && "${clean_env[@]}" ./run.sh --jitconfig "$jit") 3>&- || {
      echo "::error::official runner $name-$serial exited non-zero"
      return 1
    }
  done
}

probes_done() {
  local jobs consumers lifecycle pending
  jobs=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/jobs?per_page=100" \
    --jq ".jobs[] | select(.name | test(\"^(probe|negative|lifecycle)-$PLATFORM-\")) | \"\\(.status) \\(.name)\"") || return 1
  consumers=$(printf '%s' "$jobs" | grep -c ' / consumer$' || true)
  lifecycle=$(printf '%s' "$jobs" | grep -c "^completed lifecycle-$PLATFORM-" || true)
  pending=$(printf '%s' "$jobs" | grep -vc '^completed ' || true)
  echo "$PLATFORM: $consumers of $CONSUMER_JOBS consumer jobs listed, $lifecycle of $LIFECYCLE_JOBS lifecycle jobs completed, $pending jobs not completed"
  [[ "$consumers" -ge "$CONSUMER_JOBS" && "$lifecycle" -ge "$LIFECYCLE_JOBS" && "$pending" -eq 0 ]]
}

stop_lanes() {
  local lane
  # The loop first, or it would mint a new runner when the listener dies.
  kill ${official_pid:+"$official_pid"} ${toolu_pid:+"$toolu_pid"} ${extra_pids[@]+"${extra_pids[@]}"} 2>/dev/null || true
  pkill -f "$OFFICIAL_DIR" || true
  pkill -f "$RUNNER_TEMP/official-pool-" || true
  if [[ -n "$accelerated" ]]; then
    docker logs "$ACCELERATED_CONTAINER" >"$RUNNER_TEMP/toolu-accelerated.log" 2>&1 || true
    docker rm -f "$ACCELERATED_CONTAINER" >/dev/null 2>&1 || true
  fi
  wait 2>/dev/null || true
  for lane in toolu official ${accelerated:+toolu-accelerated} ${extra_logs[@]+"${extra_logs[@]}"}; do
    echo "::group::$lane lane log"
    cat "$RUNNER_TEMP/$lane.log" || true
    echo "::endgroup::"
  done
}

# Registers the forwarder toolu lane (own home) and starts it in the background;
# call it directly (not in `$(…)`) so the lane stays this shell's child for `wait`.
start_toolu() {
  register_toolu toolu-home "$LABEL_PREFIX-toolu" "$LABEL_PREFIX-toolu" "$RUNNER_TEMP/toolu-home/_work"
  run_toolu toolu-home toolu
  toolu_pid=$!
}

# register_toolu <home> <name> <label> <work> [extra register args…]
register_toolu() {
  local home="$RUNNER_TEMP/$1" name="$2" label="$3" work="$4"
  shift 4
  mkdir -p "$home"
  "${clean_env[@]}" "TOOLU_RUNNER_HOME=$home" "TOOLU_RUNNER_TOKEN=$GH_TOKEN" "$TOOLU_BIN" register \
    --url "https://github.com/$GITHUB_REPOSITORY" --name "$name" --labels "$label" \
    --config "$home/config.toml" --work "$work" "$@" >&2
}

# run_toolu <home> <log name>: starts `run` in the background ($! is its pid).
run_toolu() {
  local home="$RUNNER_TEMP/$1"
  "${clean_env[@]}" "TOOLU_RUNNER_HOME=$home" "TOOLU_RUNNER_TOKEN=$GH_TOKEN" "$TOOLU_BIN" run \
    --config "$home/config.toml" >"$RUNNER_TEMP/$2.log" 2>&1 &
}

# #251 lanes: pools for concurrent registrations, a fresh-workspace and an
# ephemeral toolu registration. Their pids join extra_pids (killed on exit),
# except the ephemeral one, which must exit by itself after its one job.
start_extended() {
  local pool="$LABEL_PREFIX-toolu-pool" config="$RUNNER_TEMP/toolu-fresh/config.toml" i
  for i in 1 2; do
    register_toolu "toolu-pool-$i" "$pool-$i" "$pool" "$RUNNER_TEMP/toolu-pool-work"
    run_toolu "toolu-pool-$i" "toolu-pool-$i"
    extra_pids+=("$!")
    official_lane "$RUNNER_TEMP/official-pool-$i" "$LABEL_PREFIX-official-pool" "$LABEL_PREFIX-official-pool-$i" \
      3>&1 >"$RUNNER_TEMP/official-pool-$i.log" 2>&1 &
    extra_pids+=("$!")
    extra_logs+=("toolu-pool-$i" "official-pool-$i")
  done
  register_toolu toolu-fresh "$LABEL_PREFIX-toolu-fresh" "$LABEL_PREFIX-toolu-fresh" "$RUNNER_TEMP/toolu-fresh/_work"
  sed -i.bak 's/^mode = "stable"$/mode = "fresh"/' "$config"
  grep -qx 'mode = "fresh"' "$config" || {
    echo "::error::could not set [workspace] mode = fresh; found: $(grep '^mode = ' "$config" || echo 'no mode line')"
    return 1
  }
  run_toolu toolu-fresh toolu-fresh
  extra_pids+=("$!")
  register_toolu toolu-ephemeral "$LABEL_PREFIX-toolu-ephemeral" "$LABEL_PREFIX-toolu-ephemeral" \
    "$RUNNER_TEMP/toolu-ephemeral/_work" --ephemeral
  run_toolu toolu-ephemeral toolu-ephemeral
  ephemeral_pid=$!
  extra_logs+=(toolu-fresh toolu-ephemeral)
}

# The ephemeral lane takes one job, unregisters and exits; record how it ended.
await_ephemeral() {
  local waited=0 code
  while kill -0 "$ephemeral_pid" 2>/dev/null && ((waited < 120)); do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$ephemeral_pid" 2>/dev/null; then
    echo "::warning::the ephemeral toolu lane was still running after its job"
    kill "$ephemeral_pid" 2>/dev/null || true
    code=running
  else
    wait "$ephemeral_pid" && code=0 || code=$?
  fi
  printf '%s\n' "$code" >"$RUNNER_TEMP/ephemeral.exit"
}

# The accelerated lane: register and run inside one ubuntu container. Only its own
# home is mounted, at the same path, so the registration's paths hold on both
# sides; the token goes in by name (`-e TOOLU_RUNNER_TOKEN`), never on a command line.
start_accelerated() {
  local label="$LABEL_PREFIX-toolu-accelerated" home="$RUNNER_TEMP/toolu-accelerated-home"
  mkdir -p "$home"
  TOOLU_RUNNER_TOKEN="$GH_TOKEN" docker run -d --name "$ACCELERATED_CONTAINER" \
    -v "$home:$home" -v "$TOOLU_BIN:/usr/local/bin/toolu-runner:ro" \
    -e TOOLU_RUNNER_TOKEN -e "TOOLU_RUNNER_HOME=$home" -e "LANE_LABEL=$label" \
    -e "LANE_URL=https://github.com/$GITHUB_REPOSITORY" "$ACCELERATED_IMAGE" bash -c '
      set -eu
      # The probe steps use curl, jq and shasum (perl); the runner needs CA roots.
      apt-get update -qq
      DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
        ca-certificates curl jq perl >/dev/null
      config="$TOOLU_RUNNER_HOME/config.toml"
      toolu-runner register --url "$LANE_URL" --name "$LANE_LABEL" --labels "$LANE_LABEL" \
        --config "$config" --work "$TOOLU_RUNNER_HOME/_work"
      sed -i "s/^mode = \"forwarder\"\$/mode = \"accelerated\"/" "$config"
      grep -qx "mode = \"accelerated\"" "$config" || {
        echo "::error::could not set [services] mode = accelerated for $LANE_LABEL; found: $(grep "^mode = " "$config" || echo "no mode line")"
        exit 1
      }
      exec toolu-runner run --config "$config"' >&2
}

accelerated_alive() {
  [[ "$(docker inspect -f '{{.State.Running}}' "$ACCELERATED_CONTAINER" 2>/dev/null)" == true ]]
}

run_lanes() {
  official_pid="" toolu_pid="" accelerated="" ephemeral_pid=""
  extra_pids=() extra_logs=()
  [[ "${ACCELERATED_LANE:-}" == true ]] && accelerated=true
  trap stop_lanes EXIT
  # Copied before the main official lane writes its _diag, so no log is counted twice.
  if [[ "${EXTENDED_LANES:-}" == true ]]; then
    for i in 1 2; do
      rm -rf "$RUNNER_TEMP/official-pool-$i"
      cp -R "$OFFICIAL_DIR" "$RUNNER_TEMP/official-pool-$i"
    done
  fi
  start_toolu
  [[ -z "$accelerated" ]] || start_accelerated
  official_lane 3>&1 >"$RUNNER_TEMP/official.log" 2>&1 &
  official_pid=$!
  [[ "${EXTENDED_LANES:-}" != true ]] || start_extended

  until probes_done; do
    { kill -0 "$toolu_pid" && kill -0 "$official_pid" &&
      { [[ -z "$accelerated" ]] || accelerated_alive; }; } 2>/dev/null || {
      echo "::error::a lane exited before the $PLATFORM probes finished" >&2
      exit 1
    }
    sleep "$POLL_SECONDS"
  done
  [[ -z "$ephemeral_pid" ]] || await_ephemeral
}

# Counts and booleans for the comparator; GitHub must no longer list the
# ephemeral runner by the time its job completed and its process exited.
report() {
  local gone=true
  if gh api --paginate "$RUNNERS_API?per_page=100" --jq '.runners[].name' |
    grep -qx "$LABEL_PREFIX-toolu-ephemeral"; then
    gone=false
  fi
  python3 "$(dirname "$0")/compat_paired_host_report.py" --out "$RUNNER_TEMP/host-251.json" \
    --platform "$PLATFORM" --run-id "$GITHUB_RUN_ID" --temp "$RUNNER_TEMP" --official-dir "$OFFICIAL_DIR" \
    --ephemeral-exit "$RUNNER_TEMP/ephemeral.exit" --ephemeral-runner-gone "$gone"
}

# The toolu lane's acquired `stack` message (probe variant: its image is the
# real one), sanitized on this VM; the raw capture never leaves it.
capture() {
  local out="$1" raw
  raw=$(python3 - "$RUNNER_TEMP/toolu-home/_diag/captures" <<'PY'
import json, sys
from pathlib import Path
for path in sorted(Path(sys.argv[1]).glob("*.json")):
    text = path.read_text()
    message = json.loads(text)
    if message.get("jobDisplayName") == "stack" and "0000000000000251" not in text:
        print(path)
        break
PY
  )
  [[ -n "$raw" ]] || {
    echo "::error::no captured stack probe message under the toolu lane"
    return 1
  }
  python3 "$(dirname "$0")/sanitize_capture.py" "$raw" "$out" --namespace compat_251_stack
  python3 "$(dirname "$0")/sanitize_capture.py" --scan "$out"
}

# `gh api` output is re-listed each attempt, so a failed DELETE is retried and a
# survivor fails the step by name instead of being lost.
unregister() {
  local ids id attempt
  local filter=".runners[] | select(any(.labels[]; .name | startswith(\"$LABEL_PREFIX-\"))) | .id"
  for ((attempt = 0; ; attempt++)); do
    ids=$(gh api --paginate "$RUNNERS_API?per_page=100" --jq "$filter") || ids="unknown"
    if [[ -z "$ids" ]]; then
      echo "$PLATFORM: no runner left under $LABEL_PREFIX"
      return 0
    fi
    ((attempt < UNREGISTER_ATTEMPTS)) || break
    for id in $ids; do
      [[ "$id" == unknown ]] || gh api --method DELETE "$RUNNERS_API/$id" ||
        echo "::warning::could not delete runner $id yet"
    done
    sleep "$POLL_SECONDS"
  done
  echo "::error::runners still registered under $LABEL_PREFIX: $ids" >&2
  return 1
}

case "${1:-}" in
  run) run_lanes ;;
  unregister) unregister ;;
  report) report ;;
  capture) capture "${2:?usage: $0 capture OUT}" ;;
  *)
    echo "usage: $0 run|unregister|report|capture OUT" >&2
    exit 2
    ;;
esac
