#!/bin/bash
# macos-entrypoint.sh — the `command` a Namespace macOS instance runs.
#
# Namespace macOS instances boot Namespace's OWN base image (macOS + Xcode,
# picked with instance selectors); customer code arrives as an OCI image whose
# FILESYSTEM ONLY is materialised on the instance — "ENTRYPOINT/CMD from the
# image config are not respected" (namespace.so/docs/architecture/compute/macos).
# So the image cannot declare how it starts: the caller passes
# `ApplicationRequest.command`, and this script is what it names.
#
# It exists rather than pointing `command` straight at the binary because it
# does the three things the Linux image does in its Dockerfile — and cannot do
# here, since nothing in this image is ever installed:
#
#   1. Picks a WRITABLE data dir. The materialised image tree is not one.
#   2. Links the pre-seeded Node runtimes into that data dir and puts the
#      newest on $PATH (hosted-runner parity for `run: node …` steps).
#   3. Selects the mode from the environment: TOOLU_JITCONFIG -> one-shot
#      `boot`; otherwise an existing registration -> the always-online `run`
#      loop. Same image serves an ephemeral provider instance and a persistent
#      Mac.
#
# Contract, exit codes and the caller-side instance shape: docs/macos-image.md.
set -euo pipefail

here="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
runner="${here}/toolu-runner"

if [[ ! -x "${runner}" ]]; then
  echo "toolu-runner: no executable binary at ${runner} (image built wrong?)" >&2
  exit 2
fi

# The data dir must be writable and must survive for the whole job. $HOME is
# the instance's real home (Namespace runs the application as a normal user
# with Xcode on PATH); the TMPDIR fallback covers a HOME-less context, matching
# `shared::paths::expand_tilde`'s own last resort.
home="${TOOLU_RUNNER_HOME:-}"
if [[ -z "${home}" ]]; then
  if [[ -n "${HOME:-}" ]]; then
    home="${HOME}/.toolu-runner"
  else
    home="${TMPDIR:-/tmp}/toolu-runner"
  fi
fi
mkdir -p "${home}"
export TOOLU_RUNNER_HOME="${home}"

# Seeded Node runtimes: the engine looks for them at <data_dir>/node/<version>
# (`execution::node::runtime::node_cache_dir`), which is inside the writable
# home, while the seed ships in the read-only image tree. Symlink rather than
# copy — a copy would spend ~200 MB of instance disk and seconds of wall clock
# on every job for bytes that are already local.
if [[ -d "${here}/node" ]]; then
  mkdir -p "${home}/node"
  for dir in "${here}"/node/*/; do
    [[ -d "${dir}" ]] || continue
    version="$(basename "${dir}")"
    target="${home}/node/${version}"
    # A REAL directory here is an engine-downloaded runtime of the same
    # version (`ensure_node_runtime` extracts into this exact path on a
    # persistent Mac). Leave it: it is as good as the seed, and `ln -sfn`
    # against a real directory does not replace it — it silently creates the
    # link INSIDE it, leaving <data_dir>/node/<v> without a `bin/node`.
    if [[ -e "${target}" && ! -L "${target}" ]]; then
      continue
    fi
    # -f -n, not a test-then-link: the data dir outlives the image, so the
    # link can already point at a PREVIOUS image's mount. `-e` follows the
    # link and reports false for that dangling target, so a guarded `ln -s`
    # would run against a path that exists, fail with "File exists", and — as
    # the last command of the guard — take `set -e` and the whole launcher
    # down before it ever dispatches a job.
    #
    # The link itself is best-effort — the engine downloads any runtime it
    # cannot find — so a failure here (read-only home, full disk) is a WARN,
    # never the end of the job. `if !` keeps `set -e` out of it.
    if ! ln -sfn "${dir%/}" "${target}"; then
      echo "toolu-runner: could not link seeded node ${version} at ${target};" \
        "the engine will download it if a step needs it" >&2
    fi
  done
  # The seed is the engine's per-version cache for node-TYPE actions; it never
  # reaches a step's $PATH on its own, so a `run: node …` step would die with
  # "command not found". `node/default` is written by the image build (which
  # also asserts the version it names was actually seeded), so no version
  # sorting happens here — a missing file just means no injection.
  if [[ -r "${here}/node/default" ]]; then
    # `head`, not `read`: bash's `read` returns non-zero on a final line with
    # no newline even though it assigned the value, so `|| default_node=""`
    # would discard a perfectly good version. A genuine read failure (I/O,
    # permissions) is reported rather than silently swallowed.
    if ! default_node="$(head -n 1 "${here}/node/default" 2>/dev/null)"; then
      echo "toolu-runner: could not read ${here}/node/default; not adding node to PATH" >&2
      default_node=""
    fi
    # The version is interpolated into PATH, so it is checked for shape first:
    # a corrupted marker carrying a colon would append an arbitrary directory
    # to every step's search path.
    if [[ -n "${default_node}" && ! "${default_node}" =~ ^[0-9]+(\.[0-9]+)*$ ]]; then
      echo "toolu-runner: node/default is not a version ('${default_node}'); not adding node to PATH" >&2
      default_node=""
    fi
    if [[ -z "${default_node}" ]]; then
      # Either a branch above already said why, or the marker is simply an
      # empty file — nothing to name in that case, and no node on PATH.
      :
    elif [[ -x "${here}/node/${default_node}/bin/node" ]]; then
      export PATH="${here}/node/${default_node}/bin:${PATH}"
    else
      echo "toolu-runner: seeded node '${default_node}' is unusable; not adding node to PATH" >&2
    fi
  fi
fi

# Mode 1 — one-shot. The JIT config travels in the environment, never in argv,
# where it would land in the provider's dashboard. `boot` owns the exit codes
# (0 success, 1 job failure/listener error, 2 env error, 124 deadline).
if [[ -n "${TOOLU_JITCONFIG:-}" ]]; then
  exec "${runner}" boot
fi

# Mode 2 — persistent. An image baked onto a long-lived Mac with a registration
# already under the data dir runs the always-online loop instead.
# nullglob covers the per-repo layout only — the legacy root path is a literal
# and would survive in the array whether or not the file exists, which would
# make every bare instance take this branch.
shopt -s nullglob
configs=("${home}"/runners/*/*/config.toml)
shopt -u nullglob
if ((${#configs[@]} > 0)) || [[ -f "${home}/config.toml" ]]; then
  exec "${runner}" run
fi

echo "toolu-runner: TOOLU_JITCONFIG is unset and no registration exists under ${home}" >&2
echo "  ephemeral instance: pass TOOLU_JITCONFIG in the application's env_vars" >&2
echo "  persistent host:    run 'toolu-runner register' once, then re-run this" >&2
exit 2
