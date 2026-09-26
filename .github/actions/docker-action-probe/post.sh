#!/bin/sh
set -eu

test "$(cat "$HOME/docker-home-$INPUT_MARKER")" = "$INPUT_MARKER-home"
printf '%s:post:STATE_saved=%s:STATE_pre_saved=%s\n' \
  "$INPUT_MARKER" "${STATE_saved-<unset>}" "${STATE_pre_saved-<unset>}" \
  >> "$GITHUB_WORKSPACE/docker-stages.txt"
printf 'DOCKER75|%s|post|%s|%s\n' \
  "$INPUT_MARKER" "${STATE_saved-<unset>}" "${STATE_pre_saved-<unset>}"
if [ -n "${INPUT_POST_PEER:-}" ]; then
  wget -qO - "http://$INPUT_POST_PEER" | grep -q 'Welcome to nginx'
  printf '%s-post-peer-ok' "$INPUT_MARKER" \
    > "$GITHUB_WORKSPACE/docker-network-post.txt"
fi
if [ "${INPUT_FAIL_POST:-false}" = true ]; then
  exit 19
fi
