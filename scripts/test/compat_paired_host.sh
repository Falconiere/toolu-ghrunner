#!/usr/bin/env bash
# compat_paired_host.sh — one VM's side of .github/workflows/compat-paired.yml.
#
#   run         register both lanes under per-run labels and keep them serving
#               until this platform's probe jobs have all completed
#   unregister  delete every runner carrying this platform's per-run labels
#
# Environment: GH_TOKEN (administration:write), GITHUB_REPOSITORY, GITHUB_RUN_ID,
# RUNNER_TEMP, PLATFORM (Linux-X64, Linux-ARM64, macOS-ARM64), LABEL_PREFIX, and
# for `run`: OFFICIAL_DIR, TOOLU_BIN. ACCELERATED_LANE=true adds a third lane (#237):
# a second toolu registration with `[services] mode = "accelerated"`. Each lane logs to a file under
# RUNNER_TEMP, printed on exit, so a lane still alive cannot hold the step's stdout.
set -euo pipefail

POLL_SECONDS="${POLL_SECONDS:-15}"
# One `consumer` per variant and lane. It needs every other probe job, so it ends last.
# The accelerated lane runs the probe variant only.
CONSUMER_JOBS=4
[[ "${ACCELERATED_LANE:-}" == true ]] && CONSUMER_JOBS=5
UNREGISTER_ATTEMPTS="${UNREGISTER_ATTEMPTS:-6}"
RUNNERS_API="repos/$GITHUB_REPOSITORY/actions/runners"

# The VM's own job variables (GITHUB_*, RUNNER_*, ACTIONS_*) must not reach
# either lane's steps, or they would hide or fake a difference.
clean_env=(env -i "HOME=$HOME" "PATH=$PATH" "USER=$(id -un)" "LANG=${LANG:-C.UTF-8}" "TMPDIR=${TMPDIR:-/tmp}")

# A JIT runner takes exactly one job, so the official lane mints one per job.
# toolu's `run` does the same on its own. Fd 3 is the step's stdout: the mask
# command must reach it without the credential landing in the lane's log file.
official_lane() {
  local label="$LABEL_PREFIX-official" serial=0 jit
  while :; do
    serial=$((serial + 1))
    jit=$(gh api --method POST "$RUNNERS_API/generate-jitconfig" \
      -f "name=$label-$serial" -F runner_group_id=1 -f work_folder=_work \
      -f 'labels[]=self-hosted' -f "labels[]=${PLATFORM%-*}" -f "labels[]=${PLATFORM#*-}" \
      -f "labels[]=$label" --jq .encoded_jit_config)
    echo "::add-mask::$jit" >&3
    # A job's own failure still exits 0, so non-zero means the runner is broken.
    (cd "$OFFICIAL_DIR" && "${clean_env[@]}" ./run.sh --jitconfig "$jit") 3>&- || {
      echo "::error::official runner $label-$serial exited non-zero"
      return 1
    }
  done
}

probes_done() {
  local jobs consumers pending
  jobs=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/jobs?per_page=100" \
    --jq ".jobs[] | select(.name | test(\"^(probe|negative)-$PLATFORM-\")) | \"\\(.status) \\(.name)\"") || return 1
  consumers=$(printf '%s' "$jobs" | grep -c ' / consumer$' || true)
  pending=$(printf '%s' "$jobs" | grep -vc '^completed ' || true)
  echo "$PLATFORM: $consumers of $CONSUMER_JOBS consumer jobs listed, $pending jobs not completed"
  [[ "$consumers" -ge "$CONSUMER_JOBS" && "$pending" -eq 0 ]]
}

stop_lanes() {
  local lane
  # The loop first, or it would mint a new runner when the listener dies.
  kill ${official_pid:+"$official_pid"} ${toolu_pid:+"$toolu_pid"} ${accelerated_pid:+"$accelerated_pid"} 2>/dev/null || true
  pkill -f "$OFFICIAL_DIR" || true
  wait 2>/dev/null || true
  for lane in toolu official ${accelerated_pid:+toolu-accelerated}; do
    echo "::group::$lane lane log"
    cat "$RUNNER_TEMP/$lane.log" || true
    echo "::endgroup::"
  done
}

# Registers toolu lane <name> (own home) and starts it; prints the pid.
# <mode> rewrites the `[services]` mode `register` wrote.
start_toolu() {
  local name="$1" mode="$2" label="$LABEL_PREFIX-$1" home="$RUNNER_TEMP/$1-home"
  local toolu_env=("${clean_env[@]}" "TOOLU_RUNNER_HOME=$home" "TOOLU_RUNNER_TOKEN=$GH_TOKEN")
  mkdir -p "$home"
  "${toolu_env[@]}" "$TOOLU_BIN" register --url "https://github.com/$GITHUB_REPOSITORY" \
    --name "$label" --labels "$label" --config "$home/config.toml" --work "$home/_work" >&2
  if [[ "$mode" != forwarder ]]; then
    sed -i.bak "s/^mode = \"forwarder\"\$/mode = \"$mode\"/" "$home/config.toml"
    grep -qx "mode = \"$mode\"" "$home/config.toml" || {
      echo "::error::could not set [services] mode = $mode for $label" >&2
      return 1
    }
  fi
  "${toolu_env[@]}" "$TOOLU_BIN" run --config "$home/config.toml" >"$RUNNER_TEMP/$name.log" 2>&1 &
  echo $!
}

run_lanes() {
  official_pid="" toolu_pid="" accelerated_pid=""
  trap stop_lanes EXIT
  toolu_pid=$(start_toolu toolu forwarder)
  if [[ "${ACCELERATED_LANE:-}" == true ]]; then
    accelerated_pid=$(start_toolu toolu-accelerated accelerated)
  fi
  official_lane 3>&1 >"$RUNNER_TEMP/official.log" 2>&1 &
  official_pid=$!

  until probes_done; do
    { kill -0 "$toolu_pid" && kill -0 "$official_pid" &&
      { [[ -z "$accelerated_pid" ]] || kill -0 "$accelerated_pid"; }; } 2>/dev/null || {
      echo "::error::a lane exited before the $PLATFORM probes finished" >&2
      exit 1
    }
    sleep "$POLL_SECONDS"
  done
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
  *)
    echo "usage: $0 run|unregister" >&2
    exit 2
    ;;
esac
