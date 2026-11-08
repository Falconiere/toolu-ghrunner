#!/bin/sh
# #251: prove the step ran inside the built image.
set -eu
printf 'release=%s\n' "$(cat /etc/alpine-release)" >> "$GITHUB_OUTPUT"
