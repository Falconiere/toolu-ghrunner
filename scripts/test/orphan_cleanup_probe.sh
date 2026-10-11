#!/usr/bin/env bash
# Issue #89 parity probe, run as a workflow step by .github/workflows/orphan-cleanup-89.yml.
#   tagged    — leave a stdout-holding child, a bash -c grandchild, a nohup and
#               a setsid sleeper behind; the runner's job-end sweep must
#               "Terminate orphan process" each ORPHAN89|tagged pid.
#   opted-out — run with RUNNER_TRACKING_ID='' and leave one setsid sleeper
#               that the sweep must NOT terminate.
set -euo pipefail
mode=${1:?usage: orphan_cleanup_probe.sh tagged|opted-out}
P="${RUNNER_TEMP:?RUNNER_TEMP is set by the runner}/orphan89"
mkdir -p "$P"

detach() { # <pid-file> <seconds>
  python3 -c 'import os,sys,time; os.setsid(); open(sys.argv[1],"w").write(str(os.getpid())); time.sleep(int(sys.argv[2]))' "$1" "$2" >/dev/null 2>&1 < /dev/null &
  while [ ! -s "$1" ]; do sleep 0.05; done
}

case "$mode" in
  tagged)
    printf 'ORPHAN89|id-shape|%s\n' "$(printf %s "${RUNNER_TRACKING_ID-unset}" | sed -E 's/[0-9a-f]/x/g')"
    sleep 300 & echo $! > "$P/child"
    bash -c 'sleep 301 >/dev/null 2>&1 & echo $! > "$1/grandchild"' _ "$P"
    nohup sleep 302 >/dev/null 2>&1 & echo $! > "$P/nohup"
    detach "$P/detached" 303
    while [ ! -s "$P/grandchild" ]; do sleep 0.05; done
    for name in child grandchild nohup detached; do
      printf 'ORPHAN89|tagged|%s|%s\n' "$name" "$(cat "$P/$name")"
    done
    ;;
  opted-out)
    detach "$P/opted-out" 304
    printf 'ORPHAN89|opted-out|%s|%s\n' "${RUNNER_TRACKING_ID-unset}" "$(cat "$P/opted-out")"
    ;;
  *) echo "unknown mode: $mode" >&2; exit 2 ;;
esac
