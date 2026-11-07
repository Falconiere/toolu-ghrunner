#!/bin/sh
# #251: main runs after this use's pre.
set -eu
grep -qx "$INPUT_WHO" /github/workspace/compat-251-pre-ran
printf '%s\n' "$INPUT_WHO" >> /github/workspace/compat-251-main-ran
