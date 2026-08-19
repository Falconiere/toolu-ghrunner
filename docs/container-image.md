# Container image

The repo ships a container image for [toolu.sh](https://toolu.sh)'s
runner compute providers (Namespace, Fly Machines, VPS hosts): a
one-shot GitHub Actions JIT runner that boots with **zero arguments**,
reads its whole contract from the environment, runs exactly one job, and
exits. The providers never override the image's entrypoint, command, or
arguments — the JIT config must never appear in argv, where it would
leak into a provider's dashboard or audit trail.

It publishes in **two variants**, from one `Dockerfile` and one payload:

| Variant | Build target | Tags | For |
| --- | --- | --- | --- |
| default | `runner` | `X.Y.Z`, `latest`, `vN` | Every provider. No Docker daemon. |
| Docker-capable | `docker` | `X.Y.Z-docker`, `latest-docker`, `vN-docker` | VPS hosts that opt in per host via `vps_hosts.image_ref`. Adds `dockerd` + the CLI, buildx and compose plugins. |

The daemon is a separate tag rather than an addition to the default
image because toolu.sh's `DEFAULT_RUNNER_IMAGE` follows this repo's
moving tag: putting `dockerd` in it would slow the cold pull for every
deployment, with no deploy of its own, to serve the hosts that opt in.
Everything below applies to both variants unless it says otherwise.

## Environment contract

| Variable | Format | Meaning |
| --- | --- | --- |
| `TOOLU_JITCONFIG` | GitHub's `encoded_jit_config`, verbatim | The JIT runner config minted by `POST /orgs/{org}/actions/runners/generate-jitconfig` — the same 3-blob base64 envelope `toolu-runner register` persists. Required; missing → exit 2 with no network traffic. |
| `TOOLU_DEADLINE` | Epoch **milliseconds**, decimal string (`/^\d+$/`) | Self-termination backstop for hung jobs. **Not** a promise of remaining time: Namespace may clamp the real instance deadline *earlier* server-side without updating this value. Missing or unparseable → warn and run without a watchdog. Already past at boot → exit 124 immediately. |
| `TOOLU_DOCKERD_TIMEOUT` | Whole seconds, decimal string (`/^\d+$/`) | **Docker-capable variant only.** How long the entrypoint waits for the daemon's API before giving up on it and dispatching the job anyway. Default `60`; unparseable → warn and use the default. Never fails a job by itself. |

No other variables are read from the provider. `TOOLU_RUNNER_HOME` is
preset in the image to `/var/lib/toolu-runner` (the data dir holding
the pre-seeded Node runtimes, `_diag/` logs, the `_work/` workspace,
and — in the Docker-capable variant — `dockerd.log`).

### Behavior

The default variant's `ENTRYPOINT` is `toolu-runner boot`; the
Docker-capable one wraps it in
[`scripts/docker-entrypoint.sh`](../scripts/docker-entrypoint.sh),
which starts the daemon first and then `exec`s the same command, so the
description below is the contract for both. `boot`: parse `TOOLU_JITCONFIG`
straight into the listener (no `register`, no `config.toml`, no
credential store, no `.lock`), poll for the one job the JIT config
exists for, run it, report to GitHub, exit. There is no re-mint loop —
a JIT config is single-use by design, and the providers destroy the
instance after the job (`restart.policy: "no"` + `auto_destroy` on
Fly; deadline-driven teardown on Namespace).

When a deadline is set, a watchdog fires at that instant: it cancels
the in-flight job gracefully (the job reports Cancelled to GitHub) and
hard-exits after a 30-second grace period. The graceful path normally
wins — once the listener returns, `boot` stands the watchdog down and
exits `124` through its own return path, so the hard exit is reached
only when shutdown itself hangs.

That hard exit is `std::process::exit(124)`, and it is deliberately
brutal: it runs no destructors, so anything still in flight — the
journal writer, a step's log upload, the `/completejob` report — is
abandoned mid-write. The runner flushes stderr immediately before it
so the reason survives in the provider's log, and the `_diag` sink is
synchronous (`tracing_appender::rolling`, no background worker) so
what was already logged is on disk. Nothing else is guaranteed. This
is the intended trade: a deadline backstop cannot wait on the very
teardown that hung, and the provider force-kills the instance at its
own deadline regardless. A job that reaches this path may appear
in-progress in the GitHub UI until GitHub times it out.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Job completed with Success or Skipped, or the runner shut down (signal or session end) before any job was acquired. |
| `1` | Job completed with Failure or Cancelled, or the listener failed (auth, network, protocol). |
| `2` | Environment error before polling: `TOOLU_JITCONFIG` missing or unparseable. |
| `124` | The `TOOLU_DEADLINE` watchdog fired (including a deadline already in the past at boot). |

Providers don't inspect exit codes (their lifecycle is webhook- and
deadline-driven); the codes exist for humans reading provider logs.

The Docker-capable variant adds none of its own. Its entrypoint `exec`s
the runner, so the code above is what the provider sees — a daemon that
failed to start changes the job log, never the outcome. The one
exception is the existing `2`: an image with no `toolu-runner` on
`$PATH` is an env error, reported as one.

## What's inside

- `debian:bookworm-slim`, built from `rust:1.94.1-bookworm` (suites
  pinned together so builder glibc never exceeds the runtime's).
- Multi-arch: `linux/amd64` (what Namespace/Fly request) and
  `linux/arm64` (local runs on Apple-Silicon Macs — the container VM
  executes the arm64 leg natively). One manifest-list digest covers
  both.
- `bash` (the engine's default step shell), `git` (checkout actions
  shell out to it), `ca-certificates` (the workspace is rustls-only —
  no OpenSSL in the image), plus `curl jq unzip tar gzip zstd xz-utils
  sudo` for the long tail of workflow `run:` scripts.
- Node **20.18.3** and **24.0.2** pre-seeded at
  `/var/lib/toolu-runner/node/<version>/` — the versions
  `crates/execution/src/node/runtime.rs` resolves for `node20` /
  `node24` actions. Ephemeral containers never warm a lazy cache, so
  seeding removes the per-job nodejs.org fetch (and the egress
  requirement) entirely.
- The newest seeded LTS (**24.0.2**) is also on **PATH** as `node` /
  `npm` / `npx` / `corepack` (symlinked into `/usr/local/bin`), matching
  GitHub-hosted images: `run:` steps that invoke `node` directly — the
  standard composite-action shape — resolve it. The per-version seed
  alone only serves node-**type** actions and never reaches a step's
  `$PATH`.
- Runs as **root**, with `sudo` installed as a passthrough: isolation
  is the single-tenant micro-VM, not the container user, and root keeps
  the pervasive `apt-get` / `sudo apt-get` workflow idioms working.
- **Docker-capable variant only:** `docker-ce`, `docker-ce-cli`,
  `containerd.io`, `docker-buildx-plugin` and `docker-compose-plugin`
  from Docker's own apt repository (bookworm's `docker.io` is 20.10 and
  ships neither `buildx` nor `compose`, which is most of what a `run:`
  step actually invokes). Nothing else differs — same binary, same Node
  seed, same user.

### Limitations (all providers)

- **No Docker in the default image**: a `run:` step that calls `docker`
  fails with "command not found". The Docker-capable variant is the
  answer where a host can offer one; Cloudflare Containers has no
  docker-in-docker at all, so this stays the floor across providers
  rather than an image choice.
- **`uses: docker://…` container actions, job-level `container:` and
  `services:` are unsupported in BOTH variants.** That is an engine
  gap, not an image one — the handler is not wired, so a daemon does
  not change it. A `docker://` step fails that step with an explicit
  "docker actions not yet supported" log line; it does not take down
  the runner.
- github.com JIT configs only (matching toolu.sh's minting path);
  GHES registrations use the normal `register` / `run` flow instead.
- No macOS/Windows jobs — the image is Linux; `runs-on` labels must
  route Linux jobs to it.

## The Docker-capable variant

Published as `:<version>-docker` / `:latest-docker` / `:vN-docker` and
selected per VPS host through `vps_hosts.image_ref` — never by
`DEFAULT_RUNNER_IMAGE`. It exists so `run:` steps that shell out to
Docker (`docker build`, `docker compose`, testcontainers) work on hosts
that can isolate a daemon.

**Isolation is the host's, not the image's.** The container runs under
`sysbox-runc`, which gives it a kernel-isolated daemon of its own. The
image mounts **no** host Docker socket and needs **no** `--privileged`;
a job cannot reach the host's Docker or another tenant's containers. On
a host without sysbox this variant is not safe to run: the isolation is
a property of the runtime the toolu compute daemon assigns to the job
container, not of this tag. Getting a box from bare metal to a
`vps_hosts` row that can actually serve this variant — ordering it,
verifying sysbox support, registering `sysbox-runc` as a Docker
runtime, and everything after — is
[`docs/vps-host-runbook.md`](vps-host-runbook.md).

**Startup** ([`scripts/docker-entrypoint.sh`](../scripts/docker-entrypoint.sh),
pinned by
[`scripts/test/docker_entrypoint_test.sh`](../scripts/test/docker_entrypoint_test.sh)):

1. Start `dockerd` in the background, with its stdout and stderr going
   to `$TOOLU_RUNNER_HOME/dockerd.log` — daemon chatter interleaved
   with a step's output reads as a job failure.
2. Poll `docker version` until the API answers, bounded by
   `TOOLU_DOCKERD_TIMEOUT` (default 60s). A daemon that has already
   exited ends the wait immediately rather than burning the timeout.
3. `exec toolu-runner boot`. The runner becomes PID 1 and its exit code
   reaches the provider verbatim.

**A Docker failure never fails a job.** If the daemon does not come up,
the entrypoint logs one line naming the reason and dispatches the job
anyway: most workflows never touch Docker, and failing them over a
daemon they do not use turns one host's misconfiguration into a wall of
red builds. Steps that *do* call `docker` then fail on their own, in
the step that called it.

## Build, run, publish

Build locally (current arch):

```sh
docker build -t toolu-ghrunner:dev .                            # default
docker build --target docker -t toolu-ghrunner:dev-docker .     # + dockerd
```

The default target is the lean image: the `runner` stage is deliberately
**last** in the `Dockerfile`, so a bare `docker build .` can never
produce the fat one by accident.

Run one job against a freshly minted JIT config:

```sh
docker run --rm \
  -e TOOLU_JITCONFIG="$(gh api -X POST \
      orgs/<org>/actions/runners/generate-jitconfig \
      -f name=smoke -F runner_group_id=1 -f 'labels[]=self-hosted' \
      --jq .encoded_jit_config)" \
  -e TOOLU_DEADLINE="$(( ($(date +%s) + 21600) * 1000 ))" \
  toolu-ghrunner:dev
```

Publishing is tag-driven: pushing `vX.Y.Z` runs
[`release-image.yml`](../.github/workflows/release-image.yml), which
gates on fmt/clippy/tests, then builds **each variant on each arch** on
a native runner (`ubuntu-24.04` / `ubuntu-24.04-arm` — no QEMU Rust
builds), pushes every leg by digest, and merges each variant's two legs
into its own manifest at `ghcr.io/<owner>/toolu-ghrunner:<version>` and
`…:<version>-docker` (plus `:latest` / `:vN` and their `-docker`
counterparts for stable tags). Each manifest job prints its
**manifest-list digest** in the log and job summary:

```
ghcr.io/<owner>/toolu-ghrunner@sha256:…
```

The default variant's digest-pinned ref is what toolu.sh operators set
as `NAMESPACE_RUNNER_IMAGE` / `FLY_RUNNER_IMAGE`; the `-docker` one
goes in a VPS host's `image_ref`. Always pin the digest, never a
mutable tag — this container runs customer build code holding a live
GitHub credential. PRs touching `Dockerfile`, `.dockerignore`,
`scripts/docker-entrypoint.sh` or the workflow build both variants
without pushing; `workflow_dispatch` does the same on demand.

### Tag surface

| Tag | Moves? | For |
| --- | --- | --- |
| `@sha256:…` | never | **Provider config.** The only ref that pins what runs. |
| `X.Y.Z` | never in practice | A specific release — reproducing a run, bisecting. |
| `vN` | on each stable release in the line | Humans: `docker run … :v6` to follow the current compat line. |
| `latest` | on each stable release | Quick local smoke tests. Never a provider. |
| `…-docker` | as its unsuffixed twin does | The Docker-capable variant of that exact tag. A VPS host pins the digest; the moving ones are for humans. |

`vN` follows the semver **compatibility boundary**, which is not always
the major: while the major is `0` the minor is what breaks, so `0.6.x`
publishes `:v6` and `0.7.x` publishes `:v7`; from `1.0.0` on it is the
major, so `1.x` publishes `:v1`.

The number therefore **resets once at 1.0.0** — `:v1` is *newer* than
`:v7`, and the `vN` series is not sortable across that boundary. Read
`vN` as the name of a compatibility line, never as an ordering. If that
ambiguity would bite a consumer, they should be on a digest anyway.

Prereleases (`vX.Y.Z-rc.1`) get the exact-version tag only — never
`latest`, never `vN`. A tag consumers follow must not move onto an
unreleased build.

One-time setup per registry namespace: flip the GHCR package to
**public** (Fly cannot pull from private registries; alternatively
re-push the image to `registry.fly.io`).
