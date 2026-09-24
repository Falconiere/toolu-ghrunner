#!/usr/bin/env bash
# Disposable probe values; no credentials. Run by the real job shell.
set -eu
if [[ ${1:-} == register ]]; then
  printf '%s\n' '::add-mask::' '::add-mask:: %0D%0A  %0D '
  printf '%s\n' '::add-mask:: Q %0ARS%0D%0A TUV %0D probe-overlap %0Aprobe-overlap-tail%0D%0Aprobe-overlap%0A%0A'
  printf '%s\n' '::add-mask::probe%250Aliteral%0A Ω '
  exit 0
fi
# Each writer's line is shorter than PIPE_BUF. Order may differ, content cannot.
for value in Q RS TUV probe-overlap probe-overlap-tail 'probe%0Aliteral' Ω; do
  (printf 'stdout=[%s]\n' "$value") &
  (printf 'stderr=[%s]\n' "$value" >&2) &
done
wait
printf '%s\n' 'control remains visible'
