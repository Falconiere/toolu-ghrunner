#!/bin/sh
# #251: post keeps its input and pre state, follows its main, and exactly the
# `before` uses have posted first (last-in, first-out).
set -eu
test "$STATE_from_pre" = "$INPUT_WHO"
grep -qx "$INPUT_WHO" /github/workspace/compat-251-main-ran
seen=$(cat /github/workspace/compat-251-posts 2>/dev/null | tr '\n' '|')
test "$seen" = "$INPUT_BEFORE"
printf '%s\n' "$INPUT_WHO" >> /github/workspace/compat-251-posts
