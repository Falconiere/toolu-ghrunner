#!/bin/sh
set -eu

marker=${INPUT_MARKER:-registry}
test "$GITHUB_WORKSPACE" = /github/workspace
test "$HOME" = /github/home
test "$RUNNER_TEMP" = /github/runner_temp
test -f "$GITHUB_EVENT_PATH"
test -d "$GITHUB_ACTION_PATH"
test -S /var/run/docker.sock
printf '%s-home\n' "$marker" > "$HOME/docker-home-$marker"
{
  printf '%s:main\n' "$marker"
  printf 'argc=%s\n' "$#"
  index=0
  for arg in "$@"; do
    printf 'arg%s=<%s>\n' "$index" "$arg"
    index=$((index + 1))
  done
  printf 'DEFAULT_ONLY=<%s>\n' "${DEFAULT_ONLY-<unset>}"
  printf 'OVERRIDE_ME=<%s>\n' "${OVERRIDE_ME-<unset>}"
  printf 'INPUT_MARKER=<%s>\n' "${INPUT_MARKER-<unset>}"
  printf 'STATE_pre_saved=<%s>\n' "${STATE_pre_saved-<unset>}"
  printf 'PATH=<%s>\n' "$PATH"
} >> "$GITHUB_WORKSPACE/docker-stages.txt"
printf 'DOCKER75|%s|main|argc=%s\n' "$marker" "$#"

printf 'probe_output=%s-output\n' "$marker" >> "$GITHUB_OUTPUT"
printf 'FROM_DOCKER=%s-env\n' "$marker" >> "$GITHUB_ENV"
mkdir -p "$GITHUB_WORKSPACE/.docker-probe-bin"
cp /probe/docker-probe-tool "$GITHUB_WORKSPACE/.docker-probe-bin/docker-probe-tool"
chmod 0755 "$GITHUB_WORKSPACE/.docker-probe-bin/docker-probe-tool"
printf '%s\n' "$GITHUB_WORKSPACE/.docker-probe-bin" >> "$GITHUB_PATH"
printf 'saved=%s-state\n' "$marker" >> "$GITHUB_STATE"
printf '::warning file=probe.sh,line=7,title=Docker probe::annotation-%s\n' "$marker"

if [ -n "${INPUT_PEER:-}" ]; then
  attempt=0
  while ! wget -qO "$GITHUB_WORKSPACE/docker-network.txt" "http://$INPUT_PEER:8080"; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 50
    sleep 0.1
  done
fi
if [ "${INPUT_SLEEP_MAIN:-false}" = true ]; then
  : > "$GITHUB_WORKSPACE/docker-cancel-started"
  sleep 300
fi
if [ "${INPUT_FAIL_MAIN:-false}" = true ]; then
  exit 17
fi
