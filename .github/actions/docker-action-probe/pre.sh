#!/bin/sh
set -eu

test "$GITHUB_WORKSPACE" = /github/workspace
test "$HOME" = /github/home
test "$RUNNER_TEMP" = /github/runner_temp
test -f "$GITHUB_EVENT_PATH"
test -S /var/run/docker.sock
printf '%s-home\n' "$INPUT_MARKER" > "$HOME/docker-home-$INPUT_MARKER"
printf '%s:pre\n' "$INPUT_MARKER" >> "$GITHUB_WORKSPACE/docker-stages.txt"
printf 'pre_saved=%s-pre\n' "$INPUT_MARKER" >> "$GITHUB_STATE"
printf 'DOCKER75|%s|pre\n' "$INPUT_MARKER"
if [ "${INPUT_FAIL_PRE:-false}" = true ]; then
  exit 18
fi
