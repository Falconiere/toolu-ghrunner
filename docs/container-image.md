# Container image

The repo ships a container image for [toolu.sh](https://toolu.sh)'s
runner compute providers (Namespace, Fly Machines): a one-shot GitHub
Actions JIT runner that boots with **zero arguments**, reads its whole
contract from two environment variables, runs exactly one job, and
exits. The providers never override the image's entrypoint, command, or
arguments — the JIT config must never appear in argv, where it would
leak into a provider's dashboard or audit trail.

## Environment contract

| Variable | Format | Meaning |
| --- | --- | --- |
| `TOOLU_JITCONFIG` | GitHub's `encoded_jit_config`, verbatim | The JIT runner config minted by `POST /orgs/{org}/actions/runners/generate-jitconfig` — the same 3-blob base64 envelope `toolu-runner register` persists. Required; missing → exit 2 with no network traffic. |
| `TOOLU_DEADLINE` | Epoch **milliseconds**, decimal string (`/^\d+$/`) | Self-termination backstop for hung jobs. **Not** a promise of remaining time: Namespace may clamp the real instance deadline *earlier* server-side without updating this value. Missing or unparseable → warn and run without a watchdog. Already past at boot → exit 124 immediately. |

No other variables are read from the provider. `TOOLU_RUNNER_HOME` is
preset in the image to `/var/lib/toolu-runner` (the data dir holding
the pre-seeded Node runtimes, `_diag/` logs, and the `_work/`
workspace).

### Behavior

The `ENTRYPOINT` is `toolu-runner boot`: parse `TOOLU_JITCONFIG`
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
- Runs as **root**, with `sudo` installed as a passthrough: isolation
  is the single-tenant micro-VM, not the container user, and root keeps
  the pervasive `apt-get` / `sudo apt-get` workflow idioms working.

### Limitations (all providers)

- **No Docker inside the image**: `uses: docker://…` container
  actions, job-level `container:`, and `services:` are unsupported. A
  `docker://` step fails that step with an explicit
  "docker actions not yet supported" log line — it does not take down
  the runner. (Cloudflare Containers has no docker-in-docker at all,
  so this is a floor across providers, not an image choice.)
- github.com JIT configs only (matching toolu.sh's minting path);
  GHES registrations use the normal `register` / `run` flow instead.
- No macOS/Windows jobs — the image is Linux; `runs-on` labels must
  route Linux jobs to it.

## Build, run, publish

Build locally (current arch):

```sh
docker build -t toolu-ghrunner:dev .
```

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
gates on fmt/clippy/tests, builds each arch on a native runner
(`ubuntu-24.04` / `ubuntu-24.04-arm` — no QEMU Rust builds), pushes
both legs by digest, merges them into one manifest at
`ghcr.io/<owner>/toolu-ghrunner:<version>` (plus `:latest` and the
`:vN` compat line for stable tags), and prints the **manifest-list
digest** in the log and job summary:

```
ghcr.io/<owner>/toolu-ghrunner@sha256:…
```

That digest-pinned ref is what toolu.sh operators set as
`NAMESPACE_RUNNER_IMAGE` / `FLY_RUNNER_IMAGE`. Always pin the digest,
never a mutable tag — this container runs customer build code holding
a live GitHub credential. PRs touching `Dockerfile`, `.dockerignore`,
or the workflow build the image without pushing; `workflow_dispatch`
does the same on demand.

### Tag surface

| Tag | Moves? | For |
| --- | --- | --- |
| `@sha256:…` | never | **Provider config.** The only ref that pins what runs. |
| `X.Y.Z` | never in practice | A specific release — reproducing a run, bisecting. |
| `vN` | on each stable release in the line | Humans: `docker run … :v6` to follow the current compat line. |
| `latest` | on each stable release | Quick local smoke tests. Never a provider. |

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
