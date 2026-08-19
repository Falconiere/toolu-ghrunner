# The toolu.sh provider runner image: boots with zero arguments, reads
# TOOLU_JITCONFIG / TOOLU_DEADLINE from the environment, runs exactly one
# GitHub Actions job, and exits with the job's outcome. Contract + usage:
# docs/container-image.md. Design: docs/toolu/specs/2026-08-06-container-image-boot.md
# (gitignored; the doc above is the durable copy of the contract).
#
# Multi-arch: linux/amd64 (Namespace requests `machineArch: "amd64"`;
# Fly matches) + linux/arm64 (local runs on Apple-Silicon Macs, where the
# container VM executes arm64 natively). CI compiles each arch on a native
# runner — this file assumes no cross-compilation, only $TARGETARCH-aware
# artifact selection. Builder suite (bookworm) is pinned to the runtime
# base so the builder's glibc never exceeds the runtime's.
#
# Two published targets share one `runtime` payload:
#
#   --target runner  (default)  the lean image every provider pulls
#   --target docker             the same payload plus a Docker daemon,
#                               published under the `-docker` tag suffix
#
# `runner` is LAST on purpose: the last stage is what a bare `docker build .`
# builds, and the lean image is the one that must stay the default. Fattening
# it would slow the cold pull for every deployment — toolu.sh's
# DEFAULT_RUNNER_IMAGE follows this repo's moving tag — while only VPS hosts
# that opt in per host (`vps_hosts.image_ref`) need a daemon at all.

FROM rust:1.94.1-bookworm AS builder
WORKDIR /src
COPY . .
# --locked pins the committed Cargo.lock, same as release.yml.
RUN cargo build --release --locked --bin toolu-runner

# Pre-seed the Node runtimes the engine's `ensure_node_runtime` would
# otherwise download from nodejs.org on first use. These containers are
# ephemeral — a lazy cache never warms, so without the seed every
# node-action job pays a cold fetch and requires nodejs.org egress.
# Versions must match `node_version_for` in crates/execution/src/node/runtime.rs
# (node20 -> 20.18.3, node24 -> 24.0.2; unknown majors fall back to 20.18.3).
# Runs on $BUILDPLATFORM (download-only stage — never emulate it) and picks
# the tarball by $TARGETARCH: amd64 -> x64, arm64 -> arm64.
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS node-seed
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl \
  && rm -rf /var/lib/apt/lists/*
ARG NODE_VERSIONS="20.18.3 24.0.2"
ARG TARGETARCH
RUN set -eu; \
  case "${TARGETARCH}" in \
    amd64) node_arch=x64 ;; \
    arm64) node_arch=arm64 ;; \
    *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
  esac; \
  for v in ${NODE_VERSIONS}; do \
    tarball="node-v${v}-linux-${node_arch}.tar.gz"; \
    curl -fsSLO "https://nodejs.org/dist/v${v}/${tarball}"; \
    curl -fsSL "https://nodejs.org/dist/v${v}/SHASUMS256.txt" \
      | grep " ${tarball}\$" | sha256sum -c -; \
    mkdir -p "/seed/node/${v}"; \
    tar -xzf "${tarball}" -C "/seed/node/${v}" --strip-components=1; \
    rm "${tarball}"; \
  done

FROM debian:bookworm-slim AS runtime
# Runtime surface for customer job steps:
# - bash: the engine's default step shell (handlers/script.rs); sh rides along.
# - git: bundled actions/checkout shells out to it.
# - ca-certificates: rustls trust roots (the workspace is rustls-only; no libssl).
# - curl, jq, unzip: pervasive in workflow `run:` scripts.
# - tar, gzip, zstd, xz-utils: archive tools actions/cache-style steps expect.
# - sudo: workflows write `sudo apt-get ...`; as root it degrades to a
#   passthrough instead of "command not found". The container runs as root —
#   isolation is the single-tenant micro-VM, not the container user, and root
#   keeps apt-style steps working.
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    bash git ca-certificates curl sudo tar gzip zstd xz-utils unzip jq \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/toolu-runner /usr/local/bin/toolu-runner
# Seed lands where boot mode resolves its data dir: TOOLU_RUNNER_HOME/node/<v>.
COPY --from=node-seed /seed/node /var/lib/toolu-runner/node
ENV TOOLU_RUNNER_HOME=/var/lib/toolu-runner

# Hosted-runner parity for `run:` steps: GitHub's images ship `node` on PATH,
# and composite actions lean on that (`run: node "$GITHUB_ACTION_PATH/dist/…"`
# is the standard shape — the toolu-ghactions code-review action among them).
# The seed above is only the engine's per-version cache for node-TYPE actions;
# it never reaches a step's $PATH, so a `run: node …` step dies with
# "command not found". Expose the newest seeded LTS system-wide. `test -x`
# fails the build loudly if NODE_ON_PATH ever names a version the seed stage
# did not lay down (the PR image build catches it before publish).
ARG NODE_ON_PATH=24.0.2
RUN set -eu; \
  for tool in node npm npx corepack; do \
    src="/var/lib/toolu-runner/node/${NODE_ON_PATH}/bin/${tool}"; \
    test -x "${src}"; \
    ln -s "${src}" "/usr/local/bin/${tool}"; \
  done

# source/revision/version labels are stamped by the CI workflow's `labels:`
# (repo/sha are not knowable here); only the static ones live in the image.
LABEL org.opencontainers.image.title="toolu-ghrunner" \
  org.opencontainers.image.description="One-shot GitHub Actions JIT runner for toolu.sh compute providers"

# --- the Docker-capable variant -------------------------------------------
# Opt-in per VPS host (`vps_hosts.image_ref`), never the default tag. The
# container runs under `sysbox-runc`, which gives it a kernel-isolated
# daemon of its own: no host socket is mounted in and no `--privileged` is
# needed, so a job's `docker build` / `docker compose` / testcontainers step
# cannot reach the host's Docker or its other tenants.
#
# Docker's own apt repository rather than bookworm's `docker.io` (20.10):
# `docker buildx` and `docker compose` are what workflow steps actually
# invoke, and neither ships in the distro package. apt verifies every package
# against the keyring fetched here, so the fetch is the only trust step.
FROM runtime AS docker
ARG TARGETARCH
RUN set -eu; \
  apt-get update; \
  apt-get install -y --no-install-recommends ca-certificates curl; \
  install -m 0755 -d /etc/apt/keyrings; \
  curl -fsSL https://download.docker.com/linux/debian/gpg \
    -o /etc/apt/keyrings/docker.asc; \
  chmod a+r /etc/apt/keyrings/docker.asc; \
  printf '%s\n' \
    "deb [arch=${TARGETARCH} signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable" \
    > /etc/apt/sources.list.d/docker.list; \
  apt-get update; \
  apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin; \
  rm -rf /var/lib/apt/lists/*
# The daemon has to be started by something, and that something must still
# hand back the runner's exit code verbatim — see the script's header and
# scripts/test/docker_entrypoint_test.sh.
#
# `--chmod`, not a COPY plus a `RUN chmod`: the mode is set as the layer is
# written, so the entrypoint is executable even out of a checkout that lost
# the bit, and no second copy of the file is layered on top to fix it.
COPY --chmod=0755 scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
ENTRYPOINT ["docker-entrypoint.sh"]

# --- the default image -----------------------------------------------------
# Last stage on purpose: a bare `docker build .` must produce the lean image.
# Providers pass no entrypoint/cmd/args override (the JIT config must never
# reach argv — it would leak into provider dashboards). `boot` reads the env.
FROM runtime AS runner
ENTRYPOINT ["toolu-runner", "boot"]
