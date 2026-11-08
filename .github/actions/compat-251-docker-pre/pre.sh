#!/bin/sh
# #251: pre sees its input, leaves state for post and a marker every step can read.
set -eu
test -n "$INPUT_WHO"
printf 'from_pre=%s\n' "$INPUT_WHO" >> "$GITHUB_STATE"
printf '%s\n' "$INPUT_WHO" >> /github/workspace/compat-251-pre-ran
