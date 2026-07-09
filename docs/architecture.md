# toolu-runner Architecture

**Date:** 2026-06-18 · **Status:** v0.1.0 · **Author:** toolu

This document is the architectural reference for `toolu-runner`. It
mirrors the layout of the source tree and explains how the three
crates fit together, how the listener lifecycle runs, and how the
runner behaves under failure.

For the design rationale and the yamless extraction story, see
[docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md](toolu/specs/2026-06-18-toolu-runner-standalone-design.md).

## Overview

```
toolu-ghrunner/                            workspace root
├── shared/                                cross-cutting types + tracing init
│   ├── config.rs                          RunnerConfig (data_dir, workspace_root, cgroup_path)
│   ├── error.rs                           RunnerError enum
│   ├── events.rs                          RunnerEvent, ListenerEvent, Conclusion
│   ├── job_message/                       AgentJobRequestMessage + ~12 message types
│   ├── paths.rs                           expand_tilde (HOME → USERPROFILE → fallback)
│   ├── startup.rs                         tracing init + SecretRedactor + WARN about YAMLESS_*
│   └── tests/                             unit tests
├── protocol/                              SYNC, NO I/O, NO NETWORK
│   ├── auth.rs                            RSA key + JWT (PS256) crypto
│   ├── jit_config.rs                      3-blob base64 envelope parser
│   ├── messages.rs                        BrokerMessage + AES-256-CBC decrypt
│   ├── session.rs                         CreateSessionRequest/Response builders
│   ├── types.rs                           RunnerSettings, CredentialData, RsaKeyParams
│   ├── v1/                                GHES V1 types + resolve_service_url
│   └── tests/integration.rs               sync crypto tests against fixtures
└── toolu-runner/                          lib + bin (the runner)
    ├── src/main.rs                        clap CLI: register / run / remove / status / watch
    ├── src/lib.rs                         Runner struct, execute_job
    ├── src/config.rs                      TOML + JSON config load/save
    ├── src/lockfile.rs                    single-job fs2 lock
    ├── src/net/                           async I/O (reqwest)
    ├── src/listener/                      JIT lifecycle
    ├── src/reporting/                     domain types + async wrappers
    ├── src/execution/                     job engine (handlers, expressions, workflow)
    ├── src/docker/                        bollard wrapper
    ├── src/node/                          Node.js runtime detection + cache
    ├── src/plugin/                        RunnerPlugin trait + registry
    ├── src/journal/                       per-job JSONL event journal (types, writer, reader)
    ├── src/watch/                         `watch` TUI (state reducer, ratatui ui, input)
    ├── src/types/                         RunnerConfig (duplicates shared::RunnerConfig for crate-local imports)
    └── tests/                             5 integration tests (CLI, listener, failure modes, storage)
```

**Dependency direction (strict):**

```
shared  ←  protocol  ←  toolu-runner
                 ↑           ↑
                 └───────────┘
                       ↑
                 toolu-runner::net (HTTP transport)
                 toolu-runner::listener (JIT lifecycle)
                 toolu-runner::reporting (domain types)
                 toolu-runner::execution (job engine)
```

`protocol` never depends on `toolu-runner`. `shared` never depends
on `protocol`. The arrow `protocol → toolu-runner::net` is one-way
async I/O only; `protocol` itself is sync and free of network code.

## crate: `shared`

Pure types and a thin tracing init. No `tokio`, no `reqwest`, no
`bollard`. The whole crate compiles in under a second and has no
runtime dependencies beyond `serde`, `chrono`, and `tracing`.

- `RunnerConfig` — the only env-level config struct: data dir,
  workspace root, optional cgroup path. Always `None` for cgroup in
  v1.
- `RunnerError` — the single error enum threaded through the
  codebase. Variants map to categories: `Protocol`, `Auth`,
  `Network`, `Config`, `StepExecution`, `ScriptHandler`,
  `Expression`, `Docker`, `Oidc`, `Artifact`, `Cache`,
  `ReusableWorkflow`, `Reporting`, `WorkspaceInit`, `LockHeld`,
  `Cancelled`, plus `Io` and `Json` for ergonomic `From` conversions.
- `RunnerEvent` — events emitted by `Runner::execute_job()`. Carries
  the conclusion on `JobCompleted`.
- `ListenerEvent` — wraps `RunnerEvent` plus protocol-layer events:
  `SessionCreated`, `JobAcquired`, `LockRenewed`, `ReportedStatus`.
- `startup::init` / `startup::init_with_redactor` — the tracing
  layer. Two sinks: pretty stderr, JSON file
  (`data_dir/_diag/<service>.log`, daily-rotated). `EnvFilter` is
  built from `TOOLU_RUNNER_LOG` → `RUST_LOG` → `info`.
- `startup::warn_about_yamless_env` — scans the env for `YAMLESS_*`
  keys and emits a `WARN` per key to stderr. AC #23 — yamless users
  re-registering get a clear "this var is no longer recognized"
  signal.
- `startup::RedactingMakeWriter` / `RedactingWriter` — byte-level
  line splitter + `SecretRedactor` hookup. The `SecretMasker` from
  `toolu-runner` implements `SecretRedactor` so registered secrets
  never reach the JSON log file unredacted.

## crate: `protocol`

Strictly sync, no I/O, no network. The `Cargo.toml` dependency list
is the spec: only data formats (`serde*`, `base64`, `uuid`) and
crypto (`jsonwebtoken`, `num-bigint-dig`, `pkcs1`, `sha1`, `sha2`,
`aes`, `cbc`). CI enforces this — `cargo tree -p protocol` must
contain zero `reqwest`, `tokio`, `opendal`, `bollard`, `axum`.

- `auth::parse_rsa_private_key` — reconstructs PKCS#1 DER from
  `.NET RSACryptoServiceProvider` base64 parameters (modulus,
  exponent, d, p, q), computes CRT (exponent1, exponent2,
  coefficient), produces a valid PKCS#1 key.
- `auth::build_jwt` — PS256-signed JWT with the standard GitHub
  Actions runner claims: sub/iss = clientId, aud =
  authorizationUrl, jti = uuid, nbf/iat = now − 30s, exp = now + 4m30s.
- `jit_config::JitConfig::parse` — decodes the 3-blob base64
  envelope (`.runner` / `.credentials` / `.credentials_rsaparams`)
  into `RunnerSettings`, `CredentialData`, `RsaKeyParams`.
- `session::build_session_request` — builds the ephemeral
  `00000000-0000-0000-0000-000000000000` session with hostname +
  PID as the owner name. Pure function.
- `messages::decrypt_message_body` — AES-256-CBC decrypt + PKCS#7
  padding strip + BOM strip. The async `poll_message` /
  `acknowledge_message` live in `toolu-runner::net`.
- `types::*` — the three sub-blob shapes. `RunnerSettings` uses
  PascalCase (`AgentId`, `ServerUrlV2`), `RsaKeyParams` uses
  camelCase (`exponent`, `modulus`, `inverseQ`). The
  `string_or_i64` deserializer accepts GH's habit of sending
  integer-as-string for `AgentId` / `PoolId`.
- `v1::*` — GHES V1 protocol types (`ConnectionData`,
  `LocationServiceData`, `TimelineRecord`, etc.) plus the pure
  `resolve_service_url` helper. Async `V1ServiceDiscovery::discover`
  is in `toolu-runner::net`.

Tests against `protocol` need no clock, no HTTP client, no tokio —
they construct a fake `RsaKeyParams` / `JitConfig`, call the parse /
crypto functions, and assert on the bytes. The `tests/integration.rs`
suite covers RSA + JWT + JIT-config round-trips.

## crate: `toolu-runner`

The runner itself. Library + binary. Owns all async I/O, the
listener lifecycle, the execution engine, and the CLI.

### `net/` — async network layer

One-way dependency on `protocol`: takes request types from
`protocol` and `reporting`, returns either `protocol` response types
or `shared::RunnerError`. Every HTTP call in the runner flows
through this module.

- `auth::exchange_token` / `auth::authenticate` — POST to
  `authorization_url` with the PS256 JWT as
  `client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`,
  parse `AccessToken`.
- `session::create_session` / `session::delete_session` —
  `POST {server_url_v2}/session` and `DELETE …`. Status non-2xx on
  delete is logged but treated as success (broker may have already
  expired the session).
- `messages::poll_message` / `messages::acknowledge_message` — long
  poll on `GET /message?sessionId=…` (202 = no work, 200 = job),
  `DELETE /message/{id}` to ack.
- `run_service::acquire_job` / `renew_job` / `complete_job` —
  `POST /acquirejob`, `POST /renewjob`, `POST /completejob`. Reads
  `x-plan-id` and `x-actions-results-token` headers.
- `results_service::*` — Twirp RPCs (`WorkflowStepUpdateService`,
  `ResultsReceiver`). Returns signed Azure blob URLs that the
  log uploader then PUTs to.
- `log_upload::*` — Azure append-blob helpers (create / block /
  commit) used by the listener for step + job log upload.
- `v1::*` — GHES V1 `connectionData` discovery + timeline record
  POST.

### `listener/` — JIT lifecycle

The entry point is `handler::GitHubListener::run`. It wires:

1. `parse_rsa_private_key` → DER bytes (`protocol::auth`).
2. `authenticate` → `AccessToken` (`toolu-runner::net::auth`).
3. `build_session_request` + `create_session` → broker session
   (`protocol::session` + `net::session`).
4. `poll_and_execute` (long-poll loop) (`listener::job_lifecycle`).

`poll_and_execute` is a state machine over the broker responses:

- HTTP 202 → back to sleep (long-poll returned no work).
- HTTP 200 with `BrokerMigration` → switch to new broker URL,
  continue.
- HTTP 200 with `RunnerJobRequest` → acquire the job, run it,
  acknowledge, complete.
- Network error → exponential backoff (1s → 60s cap), continue.

A successful job acquires via `acquire_job`, parses the body into
`AgentJobRequestMessage`, then runs:

```
acquire_job → run_acquired_job → execute_with_renewal
            → acknowledge_message → complete_job
```

`execute_with_renewal` spawns three tasks in parallel:

1. **Renewal task** — every 60s, POST `/renewjob`. Cancel token
   breaks the loop on job completion.
2. **Event forwarder** — receives `RunnerEvent`s from the engine,
   streams log lines to the Results Service (per-step + combined
   job-level), reports step status updates, and forwards to a
   `ListenerEvent` channel for observers.
3. **Engine task** — `Runner::execute_job()` running the actual
   workflow steps. The conclusion flows back via a oneshot.

### `reporting/` — domain types + async wrappers

- `run_service::AcquireJobRequest` / `CompleteJobRequest` /
  `RenewJobRequest` and their response shapes.
- `results_service::WorkflowStepsUpdateRequest` /
  `StepUpdateEntry` (Twirp shapes).
- `feature_detection::detect` — picks V1 vs V2 from host.
- `live_log::LiveLogStreamer` — WebSocket streamer for real-time
  log lines to the GitHub Actions UI.
- `types::{Status, Conclusion, StepResult, Annotation}` — the
  Twirp value types. `Conclusion` is a `#[repr(u8)]` enum with
  the GitHub protocol integers (Success = 2, Failure = 3,
  Cancelled = 4, Skipped = 7).

### `execution/` — job engine

The job execution engine. `job_runner::run_job` is the single
entry point: it builds a `StepsRunner` (per-job step loop) and
dispatches each step to a handler:

```
StepStarted → resolve_handler(runs.using) → handler.run() → StepCompleted
```

Handler priority: **plugin → script → node → docker → composite**.

- `script` — `run: echo hello`-style shell scripts. Streams stdout /
  stderr line-by-line into `RunnerEvent::Log`.
- `node` / `node_exec` — JavaScript actions (e.g.
  `actions/checkout@v4`). Auto-downloads Node.js into
  `data_dir/_node/<version>` on first use.
- `docker` — Docker container actions via bollard. Mounts the
  workspace and streams container logs.
- `composite` — composite actions (`runs.using: composite`). Walks
  the inner steps and runs them through the same handlers.
- `plugin` — `RunnerPlugin` extension point. New addition not in
  upstream `actions/runner`.

The expression engine (`expressions/`) is a complete `${{ }}`
evaluator: lexer → parser (AST + precedence + primary) → evaluator
→ template. Function library: built-ins (`contains`, `startsWith`,
`format`, `join`, `toJSON`, `fromJSON`, `hashFiles`, etc.) plus
GH-specific helpers.

The workflow parser (`workflow/parser/`) handles the workflow YAML
shape. `reusable/` resolves `uses: org/repo/.github/workflows/x.yml`
references and propagates outputs back to the caller.

Artifacts (`artifacts/`) and cache (`cache/`) run as in-process
axum micro-services (artifact upload / download via Azure
append-blob; cache with a layered local-disk + remote backend).

### `docker/`, `node/`, `plugin/`

- `docker` — bollard wrapper. `client` builds the daemon connection,
  `services` runs service containers, `path_translator` maps host ↔
  container paths.
- `node` — `runtime::detect_version` + `runtime::download`. Caches
  Node at `data_dir/_node/<version>`.
- `plugin` — `RunnerPlugin` trait + `PluginRegistry`. Plugins can
  add custom step types and override behavior.

### `journal/` + `watch/` — local observability

The listener's `ListenerEvent` channel (every `RunnerEvent` plus
session / acquire / lock events) is sunk by `journal::writer` into
`data_dir/_diag/jobs/<UTC ts>-<job_id>.jsonl` — one JSON line per
event, secret-masked through the same `SecretMasker` as the diag log.
Line contract (v1, `journal::types`):

```json
{"v":1,"seq":12,"ts":"2026-07-08T12:34:56.789Z","type":"log","step_id":"s1","line":"hello","stream":"stdout"}
```

Envelope: `v` (contract version), `seq` (0-based, monotonic per file),
`ts` (RFC3339 UTC), then the flattened event under a snake_case
`type` tag. Readers skip unparseable / unknown-version lines; a
partial trailing line is re-read on the next poll. Pre-acquire events
are buffered (cap 256) until `job_acquired` names the file; the dir
is pruned to the newest 50 journals; a write failure WARNs once and
disables journaling without touching the job.

`toolu-runner watch` (`watch/`) is a ratatui TUI over that directory:
job history list (newest first, conclusion badges), step tree + log
tail for the opened journal (250 ms poll, 1 s rescan), and `c` →
confirm → SIGINT to the `.lock` PID (unix only) riding the existing
graceful-cancel path. Works with no runner running and no config —
it falls back to `~/.toolu-runner` for pure history browsing.

## Sequence: register

```
User                toolu-runner            GH JIT endpoint         GH API
 │                       │                       │                     │
 │ register --url ...    │                       │                     │
 │ --token ...           │                       │                     │
 ├──────────────────────>│                       │                     │
 │                       │ parse + validate URL  │                     │
 │                       │ (host must have '.')   │                     │
 │                       │                       │                     │
 │                       │ HEAD <jit endpoint>   │                     │
 │                       ├──────────────────────>│                     │
 │                       │<────── 2xx/3xx ───────┤                     │
 │                       │                       │                     │
 │                       │ write placeholder     │                     │
 │                       │ config.toml + creds   │                     │
 │                       │ (mode 0600)           │                     │
 │<──────────────────────┤                       │                     │
 │ printed: registered   │                       │                     │
 │ 'my-runner' at host   │                       │                     │
```

**v1 note:** the actual POST to the JIT endpoint with the registration
token is wired in step 10 (live smoke). For now, `register` validates
the URL, probes the JIT endpoint, and writes a placeholder config.
`run` will refuse to start until `register` is re-run against a live
GH repo.

## Sequence: run

```
SIGINT/SIGTERM        toolu-runner run             GH broker         Run Service
 │                          │                          │                  │
 │                          │ acquire .lock            │                  │
 │                          │ parse JIT config         │                  │
 │                          │ build JWT (PS256)        │                  │
 │                          │ exchange for OAuth2      │                  │
 │                          ├─────────────────────────>│                  │
 │                          │<──── AccessToken ────────┤                  │
 │                          │                          │                  │
 │                          │ create session           │                  │
 │                          ├─────────────────────────>│                  │
 │                          │<──── session_id ──────────┤                  │
 │                          │                          │                  │
 │                          │ ┌─── long-poll loop ─────┤                  │
 │                          │ │  GET /message          │                  │
 │                          │ ├───────────────────────>│                  │
 │                          │ │<─── 202 no work ────────┤                  │
 │                          │ │  GET /message          │                  │
 │                          │ ├───────────────────────>│                  │
 │                          │ │<─── 200 job message ────┤                  │
 │                          │ │                          │                  │
 │                          │ │ POST /acquirejob        │                  │
 │                          │ ├───────────────────────────────────────────>│
 │                          │ │<──── plan_id + body ───────────────────────┤
 │                          │ │                          │                  │
 │                          │ │ ┌─ execute_with_renewal │                  │
 │                          │ │ │  renew every 60s       │                  │
 │                          │ │ ├─────────────────────────── POST /renewjob│
 │                          │ │ │                          ├──────────────>│
 │                          │ │ │  report step status      │              │
 │                          │ │ ├─────────────────────────── Twirp        │
 │                          │ │ │                          ├──────────────>│
 │                          │ │ │  upload step logs        │              │
 │                          │ │ ├─────────────────────────── Azure blob  │
 │                          │ │ │  upload combined job log │              │
 │                          │ │ ├─────────────────────────── Azure blob  │
 │                          │ │<┘                          │                  │
 │                          │ │                          │                  │
 │                          │ │ DELETE /message/{id}     │                  │
 │                          │ ├───────────────────────>│                  │
 │                          │ │ POST /completejob        │                  │
 │                          │ ├───────────────────────────────────────────>│
 │                          │ │<──── ack ─────────────────────────────────┤
 │                          │ │  loop back to GET /message                │
 │                          │ └──────────────────────┤                  │
 │                          │                          │                  │
 │ ────────────────────────>│ cancel token            │                  │
 │                          │  DELETE /session        │                  │
 │                          ├───────────────────────>│                  │
 │                          │ release .lock           │                  │
 │                          │ exit                    │                  │
```

The poll loop classifies each `poll_message` outcome as one of:
`NoWork` (202), `Migrated` (BrokerMigration message), `Job`
(RunnerJobRequest), `NetworkError`, `Cancelled`. On
`NetworkError`, exponential backoff doubles (1s → 2s → … → 60s cap)
until success or cancellation. The cancellation token is wired
through every `tokio::select!` so SIGINT breaks the loop promptly.

## Sequence: cancel

Cancel from the GH UI flows through the Run Service:

```
GH UI                  Run Service              toolu-runner
 │                          │                          │
 │ "Cancel workflow run"    │                          │
 ├─────────────────────────>│                          │
 │                          │ next renew_job returns   │
 │                          │ "job cancelled"          │
 │                          ├─────────────────────────>│
 │                          │                          │ JobCompleted { conclusion: Cancelled }
 │                          │                          │ engine emits StepCompleted events
 │                          │                          │ forwarder streams last logs
 │                          │                          │ conclusion_tx.send(Cancelled)
 │                          │                          │
 │                          │                          │ acknowledge_message
 │                          │                          │ complete_job { conclusion: Cancelled }
 │                          │<─────────────────────────┤
 │                          │                          │ loop back to poll
```

The `RunnerEvent::Log` / `StepStarted` / `StepCompleted` /
`JobCompleted` events are streamed through the forwarder to the
`ListenerEvent` channel regardless of how the job terminated.
The cancellation propagates from the `cancel` token to whichever
`.await` point the handler is currently parked on.

For local cancellation (Ctrl-C), the SIGINT bridge cancels the
same token, which breaks the long-poll loop's `tokio::select!` and
propagates through the in-flight job.

## Sequence: reconnect

The runner has no explicit reconnect task. The poll loop's
`NetworkError` arm handles transient failures via exponential
backoff (1s → 60s cap). For mid-job network outages:

```
Run Service            toolu-runner             in-flight step
 │                          │                          │
 │ renewal POST             │                          │
 ├─────────────────────────>│                          │
 │                          │ renew_job fails          │
 │                          │ tracing::warn!           │
 │                          │ backoff: 1s, 2s, … 60s   │
 │                          │                          │
 │ 5 min outage             │                          │
 │                          │ step keeps running       │
 │                          │ (local process)          │
 ├─────────────────────────>│ renewal succeeds         │
 │                          │ backoff resets           │
 │                          │                          │ step ends
 │                          │ complete_job with        │
 │                          │ queued status updates    │
```

**v1 gap:** the spec requires cancelling the job with reason
"lost connection" after 5 minutes of unrecoverable network failure.
Today the runner keeps the step running locally and re-reports on
reconnect; it does not actively cancel. See
[known-bugs.md](known-bugs.md) B-001.

For session-level reconnection: on `BrokerMigration` message, the
poll loop switches `ctx.broker_url` to the new URL and continues.
For session expiry (`RunnerNotFound` 404), the listener logs a
warn and returns — the user is expected to restart `run`.

## Storage layout

```
~/.toolu-runner/
├── config.toml                 # registration + runtime config (0600)
├── credentials.json            # long-lived OAuth token (0600)
├── .lock                       # single-job lock file (0600, JSON body)
├── .pending_remove             # marker written by `remove` while a run is in flight
├── _work/                      # per-job workspaces (GitHub-style)
│   └── <repo>/
│       └── <job-id>/
├── _diag/                      # log files, diagnostic dumps
│   ├── runner.log              # JSON, secret-masked, daily-rotated
│   ├── runner.log.YYYY-MM-DD   # rotated archives
│   └── jobs/                   # per-job JSONL event journals (watch TUI)
│       └── <ts>-<job-id>.jsonl # newest 50 kept, secret-masked
└── .runner_version             # installed toolu-runner version
```

`.lock` body is JSON:

```json
{
  "pid": 12345,
  "started_at": "2026-06-18T10:00:00Z",
  "config_path": "/Users/foo/.toolu-runner/config.toml"
}
```

A second `run` reads the body, prints the PID, and exits 2. A stale
lock (holder PID dead AND mtime > 5 min) is removed and re-acquired
by the next `run`. The watcher is intentionally simple — there is no
forked watcher task; recovery happens at the next acquire attempt.

## Release pipeline

Releases are cut by `.github/workflows/release.yml`, triggered on a
`v*` tag push. The workflow reads the repo and never writes to it —
the version is human-authored in `Cargo.toml` + `CHANGELOG.md` before
tagging.

```
push tag vX.Y.Z
      │
      ▼
verify (ubuntu)      scripts/assert-version.sh — tag must equal the
      │              [workspace.package] version, else fail fast; then
      │              cargo fmt / clippy / test (the ci.yml gate).
      ▼
build (matrix ×4)    native runners, no cross: darwin/arm64 (macos-14),
      │              darwin/amd64 (macos-15-intel), linux/amd64
      │              (ubuntu-24.04), linux/arm64 (ubuntu-24.04-arm).
      │              cargo build --release --locked, then
      │              scripts/package-release.sh assembles
      │              toolu-runner-<os>-<arch>.tar.gz (binary + scripts/).
      ▼
publish (ubuntu)     sha256sum → SHA256SUMS; scripts/changelog-extract.sh
                     → release notes; gh release create (contents: write,
                     --prerelease iff the tag contains '-').
```

The asset names + tarball layout are the contract `install.sh`
consumes (`toolu-runner-<os>-<arch>.tar.gz`, binary at root, service
files under `scripts/`). glibc-dynamic only — a static musl build is
deferred because `tokio-tungstenite` pulls `native-tls` → openssl-sys.
The four release scripts are unit-tested against real repo files under
`scripts/test/` and run in `ci.yml`.

Once the release is published, two more workflows run independently
and never gate the release itself. `.github/workflows/release-finalize.yml`
(`on: release: [published, prereleased]`, `contents: read`) downloads
each target's tarball + `SHA256SUMS` straight back from the release
(not the build artifact) and verifies the checksum, a size-sanity
floor, and the `tar` member layout — catching upload corruption or a
stale `SHA256SUMS` that `publish` can't see, since `publish` only
checksums what it's about to upload, never what actually lands. It
never edits the release or the repo; a failure here is a signal, not
a rollback.

`.github/workflows/release-homebrew.yml` (`on: release: [published]`,
skipped for prerelease tags) downloads `SHA256SUMS` the same way,
renders `Formula/toolu-runner.rb` via
`scripts/generate-homebrew-formula.sh` (an `on_macos`/`on_linux` ×
`on_arm`/`on_intel` formula selecting one of the four release
tarballs), and pushes it to the external `Falconiere/homebrew-tap`
repo using a `HOMEBREW_TAP_TOKEN` fine-grained PAT — the default
`GITHUB_TOKEN` has no access outside this repo. A no-op push (formula
unchanged) is a normal outcome, not a failure. Missing the PAT fails
this workflow only; the GitHub Release is unaffected either way.

## Failure modes

The following are the v1 failure paths. Anything not listed is
non-goal for v1 (logged + best-effort; no spec guarantee).

| # | Scenario                                                      | v1 behavior |
|---|---------------------------------------------------------------|-------------|
| 1 | `run` with no `config.toml`                                   | Exit 2, point at `toolu-runner register`. |
| 2 | `run` with `config.toml` but no `credentials.json`            | Exit 2, same pointer. |
| 3 | `register` against a non-GH URL (`--url https://example.com`) | Exit 2 — runner only accepts `github.com` and GHES hosts (host must contain `.`). |
| 4 | `register` with expired / already-used token                  | Exit 2 with the GH error verbatim. |
| 5 | `register` with name that conflicts                            | Exit 2 unless `--replace`; with `--replace`, overwrites. |
| 6 | `run` started when a previous `run` holds `.lock`             | Exit 2 with the holder's PID read from the lock body. |
| 7 | `remove` mid-job                                              | Writes `.pending_remove`, refuses with exit 2 unless `--force`; with `--force`, cancels in-flight job. |
| 8 | `run` started with network down at startup                    | Blocks on poll loop, exponential backoff 1s → 60s cap. |
| 9 | `run` mid-job, network drops < 5 min                          | In-flight step keeps running locally; reporting queues and flushes on reconnect. |
| 10 | `run` mid-job, network drops > 5 min                         | **Known bug:** runner does not cancel with "lost connection". Tracked in [known-bugs.md](known-bugs.md) B-001. |
| 11 | Disk full mid-job                                            | Current step fails with `RunnerError::Io`; step marked `failure`; job completes `failure`; `run` stays alive for next job. |
| 12 | `remove` with no registration                                 | Exit 0 with "no registration found." |
| 13 | Any `YAMLESS_*` env var set                                   | Runner emits a `WARN` to stderr naming each var and ignores. |

The full failure-mode coverage lives in
`toolu-runner/tests/failure_modes_test.rs` (12 scenarios).

## Open questions

The full open-questions list is in the design spec at
[docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md](toolu/specs/2026-06-18-toolu-runner-standalone-design.md#open-questions).
The short version for v0.1.0:

1. **GHES test instance.** Not available yet. Spec covers V1; live
   GHES testing is blocked on access. Default: ship without
   GHES live-tested in v1, mark in [known-bugs.md](known-bugs.md).
2. **Homebrew tap.** Not yet. v1 uses `install.sh` + `cargo install`.
   Homebrew tap is a v1.1 fast-follow.
3. **Code signing on macOS.** Not signed in v1. Notarization in
   v1.1 if user demand warrants.
4. **Telemetry opt-in.** OTel cut for v1. A `tracing-subscriber`
   JSON-formatter + optional OTel layer (behind a feature flag) is
   a v1.1 fast-follow.