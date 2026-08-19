#!/bin/bash
# docker-entrypoint.sh — PID 1 of the image's Docker-capable variant
# (`--target docker`, published as `:<version>-docker`).
#
# The default image ships no Docker at all and boots straight into
# `toolu-runner boot` (the Dockerfile's `runner` stage, which is the default
# build target). This variant exists for VPS hosts that opt in per host via
# `vps_hosts.image_ref`: the container runs under `sysbox-runc`, so it gets
# its OWN kernel-isolated daemon. Nothing here may assume a mounted host
# socket, and nothing here needs `--privileged`.
#
# The contract it must not break is the default image's: the entrypoint's exit
# code IS the job's outcome (docs/container-image.md). So the runner is the
# LAST thing this script does and it is `exec`d — the runner's code reaches
# the provider verbatim, never a supervisor's and never dockerd's.
#
# A daemon that never comes up is a WARNING, not a failure. Most workflows
# never touch Docker, and failing them over a daemon they do not use would
# turn one host's misconfiguration into a wall of red builds. The wait is
# bounded for the same reason: hanging here would burn the job's whole
# TOOLU_DEADLINE (6 hours) before a single step ran.
#
# Contract and env vars: docs/container-image.md. Behaviour is pinned by
# scripts/test/docker_entrypoint_test.sh.
set -euo pipefail

# Seconds to wait for the daemon's API. Overridable with TOOLU_DOCKERD_TIMEOUT
# for hosts with slow storage; 60s is several times a warm sysbox start.
readonly DOCKERD_TIMEOUT_DEFAULT=60

# Squash captured output onto one line before it goes into a warning, exactly
# as scripts/macos-entrypoint.sh does: every message below is one line by
# construction so a log reader can tell this script's own diagnosis from a raw
# tool error, and a two-line dockerd message would break that.
one_line() {
  local text="${1//$'\n'/ ; }"
  printf '%s' "${text:-no error text}"
}

log() {
  echo "toolu-runner: $*" >&2
}

# The runner is the payload; without it there is no job to protect, and this
# is an image-build error rather than a job failure. 2 is the env-error code
# from the image's exit table.
if ! command -v toolu-runner >/dev/null 2>&1; then
  log "no toolu-runner on PATH (image built wrong?)"
  exit 2
fi

timeout_s="${TOOLU_DOCKERD_TIMEOUT:-${DOCKERD_TIMEOUT_DEFAULT}}"
# The value drives a loop bound, so it is checked for shape rather than fed to
# arithmetic: a non-numeric value would make `((SECONDS < deadline))` evaluate
# an unbounded expression and the wait would stop bounding anything.
if [[ ! "${timeout_s}" =~ ^[0-9]+$ ]]; then
  log "TOOLU_DOCKERD_TIMEOUT='$(one_line "${timeout_s}")' is not a whole number of seconds;" \
    "using ${DOCKERD_TIMEOUT_DEFAULT}"
  timeout_s="${DOCKERD_TIMEOUT_DEFAULT}"
fi

# Both halves are needed: dockerd to serve and the CLI to probe it. Named
# together in one line so a half-built image says which half is missing.
missing=""
for tool in dockerd docker; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    missing="${missing:+${missing} }${tool}"
  fi
done
if [[ -n "${missing}" ]]; then
  log "${missing} missing from this image; running the job without Docker" \
    "— a step that calls docker will fail"
  exec toolu-runner boot
fi

# dockerd's own output goes to a FILE, never to the job log: it is tens of
# lines of startup chatter, and interleaved with a step's output it reads as a
# job failure. The path is named in the warning below, so a real daemon
# problem stays diagnosable.
log_dir="${TOOLU_RUNNER_HOME:-/var/lib/toolu-runner}"
if ! mkdir_error="$(mkdir -p "${log_dir}" 2>&1)"; then
  log "could not create ${log_dir} ($(one_line "${mkdir_error}"));" \
    "dockerd will log under ${TMPDIR:-/tmp}"
  log_dir="${TMPDIR:-/tmp}"
  # The fallback gets the same treatment as the primary, for the same reason:
  # if it fails too, dockerd's redirect below has nowhere to land, the daemon
  # never comes up, and the warning that follows would blame the timeout for
  # a directory this script could not create. WARN-and-continue either way —
  # a job that never touches Docker still runs.
  if ! fallback_error="$(mkdir -p "${log_dir}" 2>&1)"; then
    log "could not create ${log_dir} either ($(one_line "${fallback_error}"));" \
      "dockerd has nowhere to log and is unlikely to start"
  fi
fi
dockerd_log="${log_dir}/dockerd.log"

# Defaults only: under sysbox the daemon picks a working storage driver on its
# own and listens on /var/run/docker.sock, which is where both the CLI probe
# below and the engine's own bollard client (DOCKER_HOST unset) look.
dockerd >>"${dockerd_log}" 2>&1 &
dockerd_pid=$!

started=$SECONDS
deadline=$((SECONDS + timeout_s))
ready=0
reason=""
while ((SECONDS < deadline)); do
  # A round-trip to the API, not a test for the socket file: the socket
  # appears before the daemon answers on it, so a file test would hand the
  # job a daemon that is not up yet.
  if docker version >/dev/null 2>&1; then
    ready=1
    break
  fi
  # A daemon that has already exited will never listen. Breaking here rather
  # than waiting out the timeout is what keeps a misconfigured host at a few
  # milliseconds of overhead per job instead of a full minute.
  if ! kill -0 "${dockerd_pid}" 2>/dev/null; then
    reason="exited before it accepted a connection"
    break
  fi
  sleep 0.2
done
elapsed=$((SECONDS - started))

if ((ready)); then
  log "docker daemon ready after ${elapsed}s (pid ${dockerd_pid}, log ${dockerd_log})"
else
  # The daemon's last words belong INSIDE this line rather than on a bare line
  # of their own: a raw dockerd error next to the warning reads as a step
  # failure. `tail` is best-effort — a missing log must not cost the job.
  last="$(tail -n 1 "${dockerd_log}" 2>/dev/null)" || last=""
  log "dockerd ${reason:-did not answer within ${timeout_s}s} ($(one_line "${last}"), full log" \
    "${dockerd_log}); running the job without Docker — a step that calls docker will fail"
fi

# `exec`, not a wrapper: the runner's exit code is the job's outcome and has
# to reach the provider unchanged. dockerd stays a child of PID 1 — which is
# the runner from here on — and goes away with the one-shot container.
exec toolu-runner boot
