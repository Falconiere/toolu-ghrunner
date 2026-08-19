#!/usr/bin/env bash
# docker_entrypoint_test.sh — behavioural test of scripts/docker-entrypoint.sh,
# PID 1 of the image's Docker-capable variant (`--target docker`).
#
# Nothing here is checkable by reading the image: it only ever runs inside a
# rented VPS container, one job at a time, and every failure mode looks the
# same from outside ("the runner did nothing"). So this builds a REAL tree in
# a temp dir — the entrypoint, a stub `dockerd`, a stub `docker` CLI, a stub
# `toolu-runner` — puts it on $PATH, and runs the entrypoint against it.
#
# The stubs fake exactly one thing each: being the daemon, being its client,
# being the runner binary. Everything asserted below — the exit code, how the
# runner was invoked, how long the wait took, what reached the job log — is
# the entrypoint's own real behaviour.
#
# Three properties this file exists to hold:
#
#   1. The wait for dockerd is BOUNDED. Unbounded, a daemon that never
#      listens eats the job's whole 6-hour TOOLU_DEADLINE before a step runs.
#   2. The runner's exit code is propagated VERBATIM, zero and non-zero
#      alike — it is the job's outcome (docs/container-image.md).
#   3. A Docker problem never fails a job that does not use Docker.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SRC="scripts/docker-entrypoint.sh"
fail=0

if [[ ! -f "$SRC" ]]; then
  echo "FAIL: $SRC not found" >&2
  exit 1
fi
if [[ ! -x "$SRC" ]]; then
  echo "FAIL: $SRC is not executable — the image COPYs it as the entrypoint" >&2
  fail=1
fi

TMPROOT="$(mktemp -d)"
# The stub daemon outlives the entrypoint on purpose (a real one does too), so
# it records its pid and gets reaped here rather than left running for the
# life of the shell that ran this test.
cleanup() {
  local pidfile pid
  for pidfile in "$TMPROOT"/dockerd.pid.*; do
    [[ -f "$pidfile" ]] || continue
    pid="$(cat "$pidfile")"
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
  done
  rm -rf "$TMPROOT"
}
trap cleanup EXIT

BIN="$TMPROOT/bin"
mkdir -p "$BIN"
cp "$SRC" "$BIN/entrypoint"
chmod +x "$BIN/entrypoint"

# --- the stubs -------------------------------------------------------------
# Every stub is `#!/bin/bash`, not `#!/usr/bin/env bash`: two cases below run
# with a PATH holding ONLY their own stub dir (the point of those cases is that
# this host's real docker cannot be seen), and `env bash` cannot resolve bash
# off such a PATH — the stub would die with 127 before the entrypoint's
# behaviour was ever observed.
#
# dockerd: STUB_DOCKERD_MODE picks which real daemon failure it imitates.
#   ready — comes up, then stays up (the normal case)
#   hang  — alive forever, never accepts a connection (slow/stuck storage)
#   crash — dies immediately (bad config, no kernel support)
# It writes noise to BOTH streams so the "nothing leaks into the job log"
# assertion has something to catch.
cat >"$BIN/dockerd" <<'STUB'
#!/bin/bash
echo "$$" >"${STUB_DOCKERD_PIDFILE}"
echo "INFO[0000] stub dockerd starting up, args: $*"
echo "WARN[0000] stub dockerd chatter on stderr" >&2
case "${STUB_DOCKERD_MODE:-ready}" in
  ready)
    sleep "${STUB_DOCKERD_DELAY:-0}"
    : >"${STUB_DOCKER_READY}"
    sleep 120
    ;;
  hang)
    sleep 120
    ;;
  crash)
    echo "failed to start daemon: stub refuses to run here" >&2
    exit 1
    ;;
esac
STUB
chmod +x "$BIN/dockerd"

# docker: the readiness probe. Fails until the stub daemon has signalled that
# it is serving — the same way `docker version` fails while the socket exists
# but the API is not answering yet.
cat >"$BIN/docker" <<'STUB'
#!/bin/bash
if [[ ! -e "${STUB_DOCKER_READY}" ]]; then
  echo "Cannot connect to the Docker daemon at unix:///var/run/docker.sock." >&2
  exit 1
fi
echo "Client: stub"
echo "Server: stub"
STUB
chmod +x "$BIN/docker"

# toolu-runner: reports how the entrypoint invoked it, then exits with the
# code the case asked for, so propagation is observable rather than inferred.
cat >"$BIN/toolu-runner" <<'STUB'
#!/bin/bash
echo "argv=$*"
echo "home=${TOOLU_RUNNER_HOME:-}"
echo "docker=$(docker version >/dev/null 2>&1 && echo up || echo down)"
exit "${STUB_EXIT:-0}"
STUB
chmod +x "$BIN/toolu-runner"

# --- harness ---------------------------------------------------------------
# Resolved to an absolute path, and invoked as one below: every case runs with
# a scrubbed PATH that holds only the stubs, so a bare `timeout` would not be
# found — and the cases that scrub hardest are exactly the ones that must not
# see this host's real `docker`.
TIMEOUT_BIN="$(command -v timeout)"
# Present AND runnable: an unexecutable `timeout` makes every `run_entry`
# below exit 126 without starting the entrypoint at all, and the cases that
# assert on a nonzero CODE would pass on that. Cheaper to refuse here than to
# read a green run that ran nothing.
if [[ -z "$TIMEOUT_BIN" || ! -x "$TIMEOUT_BIN" ]]; then
  echo "FAIL: a runnable coreutils 'timeout' is required — without it a hang cannot be bounded" >&2
  exit 1
fi

# run_entry <case-name> [VAR=VALUE …] — one entrypoint run in its own home,
# with only the stub bin and the system dirs on PATH (this host has a real
# `docker`; inheriting it would make cases pass for the wrong reason).
# Sets: OUT (merged stdout+stderr), CODE, ELAPSED, DOCKERD_LOG.
run_entry() {
  local name="$1"
  shift
  local home="$TMPROOT/home-$name"
  mkdir -p "$home"
  local env=(
    "PATH=$BIN:/usr/bin:/bin"
    "TOOLU_RUNNER_HOME=$home"
    "STUB_DOCKER_READY=$TMPROOT/ready-$name"
    "STUB_DOCKERD_PIDFILE=$TMPROOT/dockerd.pid.$name"
    "$@"
  )
  local start
  start="$(date +%s)"
  # A hard outer bound so a regression that hangs FAILS this test instead of
  # hanging CI. It is far above every case's own timeout, so it can only fire
  # on a genuine hang, never on a slow host.
  OUT="$(env -i "${env[@]}" "$TIMEOUT_BIN" 90 "$BIN/entrypoint" 2>&1)"
  CODE=$?
  ELAPSED=$(($(date +%s) - start))
  DOCKERD_LOG="$home/dockerd.log"
}

check() {
  local desc="$1" want="$2" got="$3"
  if [[ "$want" == "$got" ]]; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc" >&2
    echo "  want: $want" >&2
    echo "  got:  $got" >&2
    fail=1
  fi
}

# want_line <desc> <pattern> <text> — a substring the job log must carry.
want_line() {
  local desc="$1" pat="$2" text="$3"
  if grep -q -- "$pat" <<<"$text"; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc — no line matching '$pat' in: $text" >&2
    fail=1
  fi
}

# no_line <desc> <pattern> <text> — a substring the job log must NOT carry.
no_line() {
  local desc="$1" pat="$2" text="$3"
  if grep -q -- "$pat" <<<"$text"; then
    echo "FAIL: $desc — '$pat' leaked into: $text" >&2
    fail=1
  else
    echo "ok: $desc"
  fi
}

# under <desc> <seconds> — the case took less than this. The whole point of a
# bounded wait is that it ends, and only wall clock proves it.
under() {
  local desc="$1" limit="$2"
  if ((ELAPSED < limit)); then
    echo "ok: $desc (${ELAPSED}s < ${limit}s)"
  else
    echo "FAIL: $desc — took ${ELAPSED}s, expected under ${limit}s" >&2
    fail=1
  fi
}

# --- the daemon comes up: job runs, sees Docker, exits 0 -------------------
run_entry ready STUB_DOCKERD_MODE=ready
check     "a healthy daemon still boots the runner"  "argv=boot"   "$(grep '^argv=' <<<"$OUT")"
check     "…with Docker actually reachable"          "docker=up"   "$(grep '^docker=' <<<"$OUT")"
check     "…and exit 0 reaches the provider"         "0"           "$CODE"
want_line "the daemon's readiness is reported"       "docker daemon ready" "$OUT"
under     "a ready daemon costs no wait"             10

# dockerd's own chatter belongs in the log FILE, not in the job log: a step's
# output interleaved with daemon startup lines reads as a job failure.
no_line   "no dockerd stdout leaks into the job log" "stub dockerd starting up" "$OUT"
no_line   "no dockerd stderr leaks into the job log" "stub dockerd chatter"     "$OUT"
if grep -q "stub dockerd starting up" "$DOCKERD_LOG" 2>/dev/null &&
  grep -q "stub dockerd chatter" "$DOCKERD_LOG" 2>/dev/null; then
  echo "ok: both dockerd streams are captured in $DOCKERD_LOG"
else
  echo "FAIL: dockerd's output must be captured in the log file, not discarded" >&2
  fail=1
fi

# --- exit codes pass through verbatim, zero AND non-zero ------------------
# The entrypoint is now between the provider and the runner, so this is the
# contract most at risk: a supervisor that returns its own status silently
# turns every failed job green (or every green job red).
run_entry exit0 STUB_DOCKERD_MODE=ready STUB_EXIT=0
check "a succeeding runner exits 0"                 "0"   "$CODE"
run_entry exit1 STUB_DOCKERD_MODE=ready STUB_EXIT=1
check "a failing runner's 1 is propagated"          "1"   "$CODE"
run_entry exit2 STUB_DOCKERD_MODE=ready STUB_EXIT=2
check "an env error's 2 is propagated"              "2"   "$CODE"
run_entry exit124 STUB_DOCKERD_MODE=ready STUB_EXIT=124
check "the deadline code 124 is propagated"         "124" "$CODE"

# --- a daemon that never listens does not hang the job --------------------
# The daemon is alive the whole time, so nothing but the timeout can end this
# wait. TOOLU_DOCKERD_TIMEOUT is short here only to keep the suite quick; the
# bound itself is what is under test.
run_entry hang STUB_DOCKERD_MODE=hang TOOLU_DOCKERD_TIMEOUT=2 STUB_EXIT=0
under     "a daemon that never listens is waited out, not waited on" 30
check     "…and the job still runs"          "argv=boot" "$(grep '^argv=' <<<"$OUT")"
check     "…seeing no Docker"                "docker=down" "$(grep '^docker=' <<<"$OUT")"
check     "…with the runner's code intact"   "0"         "$CODE"
want_line "…and one line says why"           "did not answer within 2s" "$OUT"
want_line "…naming the consequence"          "running the job without Docker" "$OUT"

# The same case with a non-zero runner: a Docker warning must not rewrite the
# job's outcome in either direction.
run_entry hangfail STUB_DOCKERD_MODE=hang TOOLU_DOCKERD_TIMEOUT=2 STUB_EXIT=1
check "a failed job stays failed when Docker timed out" "1" "$CODE"

# --- a daemon that dies is not waited out at all --------------------------
# A long timeout with a corpse to wait on: the entrypoint must notice the exit
# and dispatch immediately, or every job on a misconfigured host pays the full
# minute before starting.
run_entry crash STUB_DOCKERD_MODE=crash TOOLU_DOCKERD_TIMEOUT=45 STUB_EXIT=0
check     "a dead daemon still boots the runner" "argv=boot" "$(grep '^argv=' <<<"$OUT")"
check     "…and exit 0 survives"                 "0"         "$CODE"
under     "…without waiting out the timeout"     20
want_line "…and the failure is named"            "dockerd exited before it accepted a connection" "$OUT"
# The daemon's own last words carry the diagnosis and belong inside that one
# line, not on a bare line beside it.
want_line "…carrying dockerd's own reason" "stub refuses to run here" "$OUT"
check "the diagnosis is one line, not two" "1" \
  "$(grep -c 'stub refuses to run here' <<<"$OUT")"
run_entry crashfail STUB_DOCKERD_MODE=crash TOOLU_DOCKERD_TIMEOUT=45 STUB_EXIT=1
check "a failed job stays failed when dockerd died" "1" "$CODE"

# --- everything the entrypoint says is prefixed and single-line -----------
# Same rule as the macOS launcher: an unprefixed line in a job log is
# indistinguishable from a step's own output.
#
# Scope: `run_entry` REASSIGNS `OUT` on every call (`OUT="$(…)"`), so this
# reads the case that ran last — the crashfail run above — not the whole
# session. That is the case with the most to say (a dockerd that died, its
# last words folded into a warning), which is why the check sits here.
stray="$(grep -vE '^(argv=|home=|docker=|toolu-runner: )' <<<"$OUT" || true)"
check "no unprefixed output reaches the job log" "" "$stray"

# --- a missing daemon binary is a warning, not a dead job -----------------
# The variant's own build could regress, or the wrong tag could be deployed
# to a Docker-expecting host. Either way the job must still run.
NODOCKER="$TMPROOT/nodocker"
mkdir -p "$NODOCKER"
cp "$BIN/toolu-runner" "$NODOCKER/toolu-runner"
cp "$BIN/entrypoint" "$NODOCKER/entrypoint"
home="$TMPROOT/home-nodocker"
mkdir -p "$home"
OUT="$(env -i "PATH=$NODOCKER" "TOOLU_RUNNER_HOME=$home" \
  "$TIMEOUT_BIN" 90 "$NODOCKER/entrypoint" 2>&1)"
CODE=$?
check     "an image with no dockerd still boots the runner" "argv=boot" "$(grep '^argv=' <<<"$OUT")"
check     "…and the job's exit code is untouched"           "0"         "$CODE"
want_line "…naming what is missing"                         "dockerd docker missing from this image" "$OUT"

# --- a runner-less image is an env error, not a crash ---------------------
# 2 is the image's env-error code; a bare shell failure would surface as 126
# or 127 and read like a job problem.
NORUNNER="$TMPROOT/norunner"
mkdir -p "$NORUNNER"
cp "$BIN/entrypoint" "$NORUNNER/entrypoint"
OUT="$(env -i "PATH=$NORUNNER" "TOOLU_RUNNER_HOME=$TMPROOT/home-norunner" \
  "$TIMEOUT_BIN" 90 "$NORUNNER/entrypoint" 2>&1)"
CODE=$?
check     "a payload-less image exits 2" "2" "$CODE"
want_line "…saying the binary is absent" "no toolu-runner on PATH" "$OUT"

# --- a corrupt timeout falls back instead of unbounding the wait ----------
# The value drives the loop bound, so a non-numeric one must not reach the
# arithmetic. Paired with a ready daemon so the fallback's 60s is never spent.
run_entry badtimeout STUB_DOCKERD_MODE=ready TOOLU_DOCKERD_TIMEOUT=soon
check     "a corrupt timeout still boots the job" "argv=boot" "$(grep '^argv=' <<<"$OUT")"
check     "…with Docker still waited for"         "docker=up" "$(grep '^docker=' <<<"$OUT")"
want_line "…and the bad value is named"           "TOOLU_DOCKERD_TIMEOUT='soon'" "$OUT"
under     "…without the wait unbounding"          20

# --- a daemon that is merely slow is waited for ---------------------------
# The bound must not be so eager that it declares a working daemon dead: this
# one answers a second after start, well inside the wait.
run_entry slow STUB_DOCKERD_MODE=ready STUB_DOCKERD_DELAY=1 TOOLU_DOCKERD_TIMEOUT=20
check "a slow daemon is waited for, not written off" "docker=up" "$(grep '^docker=' <<<"$OUT")"
check "…and the job runs after it"                   "argv=boot" "$(grep '^argv=' <<<"$OUT")"

# --- an unopenable log costs the log, never the daemon --------------------
# `dockerd >>"$log"` is a redirect, so a path that will not open is not a quiet
# loss of logging: bash prints its OWN unprefixed error into the job log and
# dockerd never starts. The fixture is a directory where the log file belongs —
# the one unwritable path that holds for every uid, including the root this
# suite may run as (`touch` on a directory succeeds, which is why the
# entrypoint probes with a real append instead).
mkdir -p "$TMPROOT/home-logblocked/dockerd.log"
run_entry logblocked STUB_DOCKERD_MODE=ready TOOLU_DOCKERD_TIMEOUT=5
want_line "an unopenable dockerd log is named"    "cannot open" "$OUT"
check     "…and Docker still comes up"            "docker=up"   "$(grep '^docker=' <<<"$OUT")"
check     "…and the job still runs"               "argv=boot"   "$(grep '^argv=' <<<"$OUT")"
check     "…with the runner's code intact"        "0"           "$CODE"
stray="$(grep -vE '^(argv=|home=|docker=|toolu-runner: )' <<<"$OUT" || true)"
check     "…and no unprefixed bash error leaks"   ""            "$stray"

if [[ "$fail" -ne 0 ]]; then
  echo "docker_entrypoint_test: FAILED" >&2
  exit 1
fi
echo "docker_entrypoint_test: all passed"
