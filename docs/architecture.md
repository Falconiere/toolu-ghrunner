# toolu-runner Architecture

**Date:** 2026-06-18 · **Status:** v0.1.0 · **Author:** toolu

This document is the architectural reference for `toolu-runner`. It
mirrors the layout of the source tree and explains how the nine
library crates plus the `toolu-runner` binary (ten workspace members
in total) fit together, how the listener lifecycle runs, and how the
runner behaves under failure.

For the design rationale, see
[docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md](toolu/specs/2026-06-18-toolu-runner-standalone-design.md).

## Overview

```
toolu-ghrunner/                            workspace root
└── crates/                                10 layered crates
    ├── protocol/                          SYNC, NO I/O, NO NETWORK
    │   ├── auth.rs                        RSA key + JWT (PS256) crypto
    │   ├── app_manifest.rs                App Manifest builder + conversion parse
    │   ├── jit_config.rs                  3-blob base64 envelope parser
    │   ├── messages.rs                    BrokerMessage + AES-256-CBC decrypt
    │   ├── session.rs                     CreateSessionRequest/Response builders
    │   ├── types.rs                       RunnerSettings, CredentialData, RsaKeyParams
    │   └── v1/                            GHES V1 types + resolve_service_url
    ├── shared/                            cross-cutting types + tracing init (no deps)
    │   ├── config.rs                      RunnerConfig, ServicesMode, CacheConfig, L2Config
    │   ├── error.rs                       RunnerError enum
    │   ├── events.rs                      RunnerEvent, ListenerEvent, Conclusion
    │   ├── job_message/                   AgentJobRequestMessage + ~12 message types
    │   ├── paths.rs                       expand_tilde + sanitize_job_id
    │   ├── platform.rs                    runner_os / runner_arch
    │   ├── secret_masker.rs               SecretMasker + MaskerRedactor
    │   └── startup.rs                     tracing init + SecretRedactor
    ├── config/                            registration config, lock, token store (→ shared)
    │   ├── config.rs                      TOML + JSON config load/save, [services]/[cache]/…
    │   ├── lockfile.rs                    single-job fs2 lock (per registration dir)
    │   ├── auth_store.rs                  keyring / 0600-file login-token store + decide_bearer TTY gate
    │   ├── app_store.rs                   GitHub App credential store (github-app.json)
    │   ├── registry.rs                    runner home + runners/<owner>/<repo>/ discovery/resolution
    │   ├── repo_infer.rs                  cwd git remote → owner/repo inference
    │   ├── remint.rs                      re-mint merge (preserve [services]/[cache]/[workspace]/[shadow])
    │   └── service_unit.rs                launchd plist / systemd unit builders (install-service)
    ├── expressions/                       the ${{ }} evaluator (→ shared)
    ├── cache/                             content-addressed CI cache (→ shared)
    ├── wire/                              async HTTP transport + reporting (→ shared, protocol)
    │   ├── net/                           async I/O (reqwest): auth, session, messages, …, register, app_manifest (+ create-app loopback callback server)
    │   └── reporting/                     Run/Results domain types, live_log, feature_detection
    ├── observability/                     journal + watch TUI (→ shared, config)
    │   ├── journal/                       per-job JSONL event journal (types, writer, reader)
    │   └── watch/                         `watch` TUI (state reducer, ratatui ui, input)
    ├── execution/                         job execution engine (→ shared, expressions, cache)
    │   ├── lib.rs                         Runner struct, execute_job
    │   ├── execution/                     job engine (job_runner, handlers, workflow, artifacts, oidc, …)
    │   ├── docker/                        bollard wrapper
    │   ├── node/                          Node.js runtime detection + cache
    │   └── plugin/                        RunnerPlugin trait + registry
    ├── listener/                          GitHub JIT lifecycle (→ execution, wire, observability, …)
    │   ├── handler.rs                     GitHubListener entry point
    │   ├── job_lifecycle.rs               long-poll loop
    │   ├── execution_loop.rs              run job + renewal + event forwarder
    │   ├── loop_decision.rs               pure next_action for the always-online run loop
    │   └── log_uploader/                  per-step + combined job-log upload
    └── toolu-runner/                      bin only (the CLI)
        ├── src/main.rs                    dispatch + the remove / watch handlers
        ├── src/cli.rs                     clap surface: register / run / remove / status / watch / install-service / login / logout / create-app
        ├── src/register_cmd.rs            register + remint_and_persist (per-repo persist, config rollback)
        ├── src/run_cmd.rs                 the always-online run loop (re-mint after each job)
        ├── src/service_cmd.rs             install-service (launchd / systemd user units)
        ├── src/login_cmd.rs               login / logout (device flow)
        ├── src/status_cmd.rs              status (no network)
        ├── src/create_app_cmd.rs          create-app (GitHub App Manifest onboarding)
        └── tests/                         integration tests across the whole graph
```

**Dependency direction (strict, acyclic):**

```
protocol ─────────────┐
                      ├──► wire ──────┐
shared ──┬────────────┘               │
         ├──► config ──► observability ├──► listener ──► toolu-runner (bin)
         ├──► expressions ──┐          │
         └──► cache ────────┴► execution
```

`protocol` and `shared` have no internal deps. `wire::net` owns all
async HTTP I/O; the arrow `protocol → wire::net` is one-way (request
types flow from `protocol`, async transport lives in `wire`).
`protocol` itself is sync and free of network code. `toolu-runner` is
a thin bin: it reaches `execution` only transitively through
`listener`.

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
- `startup::RedactingMakeWriter` / `RedactingWriter` — byte-level
  line splitter + `SecretRedactor` hookup. `shared::SecretMasker`
  (wrapped by `shared::MaskerRedactor`) implements `SecretRedactor`
  so registered secrets never reach the JSON log file unredacted.

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
  `acknowledge_message` live in `wire::net`.
- `types::*` — the three sub-blob shapes. `RunnerSettings` uses
  PascalCase (`AgentId`, `ServerUrlV2`), `RsaKeyParams` uses
  camelCase (`exponent`, `modulus`, `inverseQ`). The
  `string_or_i64` deserializer accepts GH's habit of sending
  integer-as-string for `AgentId` / `PoolId`.
- `v1::*` — GHES V1 protocol types (`ConnectionData`,
  `LocationServiceData`, `TimelineRecord`, etc.) plus the pure
  `resolve_service_url` helper. Async `V1ServiceDiscovery::discover`
  is in `wire::net`.

Tests against `protocol` need no clock, no HTTP client, no tokio —
they construct a fake `RsaKeyParams` / `JitConfig`, call the parse /
crypto functions, and assert on the bytes. The `tests/integration.rs`
suite covers RSA + JWT + JIT-config round-trips.

## crates: the runner engine + the `toolu-runner` bin

Historically a single `toolu-runner` crate, now split across the
layered graph above. `toolu-runner` itself is **bin-only** (the CLI:
`main.rs` + `cli.rs` + `register_cmd.rs` + `run_cmd.rs` +
`service_cmd.rs` + `login_cmd.rs` + `status_cmd.rs` +
`create_app_cmd.rs`). The module descriptions
that follow are grouped by their **current** crate: `net/` +
`reporting/` live in `wire`; `listener/` in `listener`; `execution/` +
`docker/` + `node/` + `plugin/` + the `Runner` (`lib.rs`) in
`execution`; `journal/` + `watch/` in `observability`; `config.rs` +
`lockfile.rs` + `auth_store.rs` in `config`.

### `net/` — async network layer (crate `wire`)

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
2. `authenticate` → `AccessToken` (`wire::net::auth`).
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
axum micro-services: artifact upload / download via Azure
append-blob, and cache as a content-addressed store (`cache/cas/`,
FastCDC chunks addressed by BLAKE3) that backs the
[accelerated mode](#accelerated-mode-cache-acceleration) described
below.

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
it discovers every registration's `runners/<owner>/<repo>/_diag/jobs`
(plus the legacy home) and browses them merged, re-discovering on each
1 s rescan so new registrations appear live.

## Accelerated mode (cache acceleration)

`[services] mode = "accelerated"` is `forwarder` plus a local cache
interception: everything still reaches real GitHub except cache
traffic, which the runner serves from local NVMe. Because the runner
owns the process that runs every step, it can host the cache service
itself and turn a network round-trip into a disk read.

### One store, both protocols

`job_runner` binds a per-job axum app on `[services] bind` (default
`0.0.0.0`, never loopback — a `docker-container` BuildKit runs in its
own network namespace and reaches the runner through host networking,
not `127.0.0.1`). The app fronts a single content-addressed store
(`cache/cas/`) through three faces:

- **v2 Twirp** — `POST /twirp/github.actions.results.api.v1.CacheService/*`
  (`CreateCacheEntry` / `FinalizeCacheEntryUpload` /
  `GetCacheEntryDownloadURL`), JSON only, proto snake_case wire names.
- **Azure-Blob-compatible endpoint** — `/_toolu/blob/*` (Put Blob /
  Put Block / Put Block List / HEAD / ranged GET), where blobs stage
  to a temp file and FastCDC chunking runs at commit.
- **legacy v1 REST** — the existing `/_apis/artifactcache/*` handlers,
  re-pointed at the same store, so `actions/cache@v4.0`–`v4.1` (which
  predate `ACTIONS_CACHE_SERVICE_V2` and speak v1) hit the accelerator
  rather than silently talking to Azure.

Chunks are FastCDC-split and BLAKE3-addressed; identical archives
collapse to shared blobs, and each chunk is re-hashed on read so
corruption can never be served. An optional S3 cold tier
(`cache/tier/l2.rs`) mirrors immutable chunks and manifests only —
never the mutable index, which stays L1-local.

### The selective reverse proxy

`ACTIONS_RESULTS_URL` is the origin for two step-facing services:
`CacheService` and `ArtifactService` (the latter used by
`upload-artifact@v4` / `download-artifact@v4`). Overriding that
variable without proxying would break artifact upload, so the app is a
**selective reverse proxy** (`cache/proxy.rs`): `CacheService/*`,
`/_apis/artifactcache/*`, and `/_toolu/blob/*` are served locally;
everything else is forwarded verbatim to the real `ACTIONS_RESULTS_URL`
with the `Authorization` header passed through untouched. The two
failure domains are independent — if upstream is unreachable the proxy
502s artifact calls only, while the local cache keeps serving.

### Trust: read-side global, write-side branch-scoped

Chunk content-verification makes cross-branch chunk sharing safe by
construction, so **reads are global**: the read ladder searches the
job's own ref, then the PR base ref, then the default branch. The
**index** — the mutable `(scope, version, key) → manifest` pointer —
keeps GitHub's branch isolation: the write scope is the job's own ref,
and a write to a *protected* branch is refused unless
`cache/trust.rs::classify_trust` returns `Trusted` (every event arm is
branch-guarded, so `workflow_dispatch` / `schedule` on a non-protected
branch cannot write a protected scope). A denied write returns
`{"ok": false, "message": "cache write denied: …"}` — a soft failure
that does not fail the job. This defends against accidental
cross-branch pollution and the network-facing attack; it does **not**
defend against arbitrary code already running on the runner (same OS
user — see the spec's threat-model boundary).

### Step env injection

`service_endpoints::forward_env` (via `job_runner::setup_job_env`)
seeds every step:

- `ACTIONS_RESULTS_URL` → `http://127.0.0.1:<port>` (local Twirp + proxy).
- `ACTIONS_CACHE_URL` → `http://127.0.0.1:<port>` (local v1 REST).
  Overriding **both** is what stops v1-only cache clients from silently
  bypassing the accelerator.
- `ACTIONS_CACHE_SERVICE_V2 = true` — modern clients prefer v2.
- `ACTIONS_RUNTIME_TOKEN` — left as the **real GitHub token**, since
  the proxy forwards it upstream; local auth is a constant-time compare
  against that same token.

Two adjacent, off-by-default subsystems ride the same config surface:
`execution/workspace_gc.rs` prunes `workspace_root/<job_id>` older than
`[workspace] gc_after_hours` (never the running job's), and
`execution/shadow/` fingerprints each `run:` step's workspace before
and after to record `would_hit` / `false_hit` to
`_diag/shadow/<job_id>.jsonl` — observation only, it never serves a
cached result.

## Sequence: register

`--url` is optional. With no arguments, `register` infers the target
repo from the cwd git remote `origin` (`config::repo_infer::detect_repo`
— github.com only; GHES hosts and org-level runners still need an
explicit `--url`). The bearer resolves `--token` > `TOOLU_RUNNER_TOKEN`
env > the stored `login` token; when none exists,
`config::auth_store::decide_bearer` gates on the terminal — interactive
stderr runs the GitHub OAuth device flow inline (the minted token is
stored at the runner-home root for next time), non-interactive fails
listing the three manual options.

```
User              toolu-runner                  api.github.com
 │                     │                               │
 │ register            │                               │
 ├────────────────────>│                               │
 │                     │ detect_repo: parse the cwd    │
 │                     │ `origin` remote               │
 │                     │ (skipped when --url is given) │
 │                     │                               │
 │                     │ resolve bearer: --token >     │
 │                     │ TOOLU_RUNNER_TOKEN > stored   │
 │                     │ login token                   │
 │                     │ decide_bearer: no token + TTY │
 │                     │  → inline device flow (code + │
 │                     │    browser, poll, store the   │
 │                     │    token at the home root)    │
 │                     │                               │
 │                     │ POST /repos/<owner>/<repo>/   │
 │                     │      actions/runners/         │
 │                     │      generate-jitconfig       │
 │                     │ (GHES --url host:             │
 │                     │  https://<host>/api/v3/…)     │
 │                     ├──────────────────────────────>│
 │                     │<─ runner id + encoded JIT cfg ┤
 │                     │                               │
 │                     │ parse the minted config       │
 │                     │ write runners/<owner>/<repo>/ │
 │                     │   config.toml + creds (0600,  │
 │                     │   all-or-nothing: config      │
 │                     │   rolled back if the creds    │
 │                     │   write fails; data_dir = the │
 │                     │   registration dir)           │
 │<────────────────────┤                               │
 │ registered 'name'   │                               │
 │ (id N) at host      │                               │
```

github.com registrations go through `api.github.com`; a GHES host keeps
its own API base. Repo URLs — inferred or explicit, github.com or GHES
— persist into the per-repo `runners/<owner>/<repo>/` dir (`--config`
overrides); org-level URLs (a single path segment) use the home-root
`config.toml` slot. The persisted `data_dir` is the registration dir
itself, so the `.lock`, `_diag/`, and the job journal all land
per-repo.

## Sequence: create-app

`toolu-runner create-app` is a one-time onboarding convenience that runs
GitHub's **App Manifest flow** to mint a user-owned GitHub App without
hand-copying an app id and PEM through the web UI. github.com only
(`--host` defaults to `github.com`; any other host errors as
unsupported).

The runner binds a loopback callback server on `127.0.0.1:0`
(`wire::net::app_manifest::CallbackServer`), builds a prefilled manifest
(`protocol::app_manifest::AppManifest::for_runner` —
`administration:write`, `public = false`, no webhook), and opens the
browser (unless `--no-browser`) to an auto-submitting form that POSTs the
manifest to GitHub. GitHub redirects back to the loopback server with a
temporary `code`, CSRF-checked against a `state` nonce
(`parse_callback_path`); the runner exchanges it at
`POST api.github.com/app-manifests/{code}/conversions`
(`convert_manifest_code`) and persists the returned credentials — app id,
PEM private key, client id/secret, webhook secret — to
`<home>/github-app.json` (`config::app_store`, 0600, shared by all
repos). It then PRINTS the app's install URL.

`--force` overwrites an existing `github-app.json`; without it the
command refuses before any network call. **Installation-token minting is
deferred:** `create-app` neither installs the app nor exchanges an
installation token this release — `register` still takes its bearer from
`--token` / `TOOLU_RUNNER_TOKEN` / the stored device-flow login.

## Multi-repo concurrency

Each registration owns its state dir, so the single-job `.lock` is per
repo: two `run`s for *different* repos hold their locks independently
and execute concurrently, while a second `run` for the *same* repo
still exits 2 with the holder's PID. `run` / `status` / `remove` pick
their registration via `config::registry::resolve_config_path`:
`--config` flag > the cwd-inferred `runners/<owner>/<repo>/config.toml`
> the sole existing registration (the legacy `<home>/config.toml`
included) > an error listing every candidate. `watch` with no usable
config browses all registrations' journals merged.

Two documented caveats for concurrent cross-repo runs:

- **`workspace_gc` residual risk.** Job workspaces share the default
  `<home>/_work` root. GC prunes job dirs older than
  `[workspace] gc_after_hours` and never the running job's own — but a
  run in one repo can prune *another* repo's still-live job dir if that
  job outlives the GC window. Rare (a job running longer than the 24 h
  default); accepted for now.
- **`service_bind` collision.** `offline` / `accelerated` service modes
  bind a local port; concurrent cross-repo runs sharing one
  `service_bind` give the second run EADDRINUSE — use a distinct
  `service_bind` per repo config. The default `forwarder` mode binds
  nothing and is unaffected.

`remove` deletes the registration's `config.toml`, `credentials.json`,
`.lock`, and `.pending_remove`, and keeps `_diag/` (the `watch`
history); empty parent dirs are left in place.

## Sequence: run

`run` is an **always-online loop**, not a one-shot:
`register` → `run` → [ job → re-mint → poll ]* → cancel. A JIT config is
single-use, so the listener runs exactly one job and returns; the bin's
`run_cmd::RunLoop` then re-mints a fresh JIT config with the stored
`login` / `TOOLU_RUNNER_TOKEN` bearer
(`register_cmd::remint_and_persist`, preserving the operator's
`[services]` / `[cache]` / `[workspace]` / `[shadow]` sections verbatim)
and builds a new listener. The per-repo `.lock` is held for the whole
loop; `--once` opts out (one job, then exit with its status).

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
 │                          │ └─ listener returns Ok — JIT config spent ─┤
 │                          │                          │                  │
 │                          │ next_action = Reregister → RE-MINT:        │
 │                          │  POST generate-jitconfig with the stored   │
 │                          │  login / TOOLU_RUNNER_TOKEN bearer         │
 │                          ├──────────────────────>│ api.github.com     │
 │                          │<─ fresh JIT config; persist, preserving ──┤ │
 │                          │  [services]/[cache]/[workspace]/[shadow]   │
 │                          │  ⟳ build a new listener, loop to the top   │
 │                          │                          │                  │
 │ ────────────────────────>│ cancel token (or .pending_remove seen      │
 │                          │  between jobs)          │                  │
 │                          │  DELETE /session        │                  │
 │                          ├───────────────────────>│                  │
 │                          │ release .lock; exit 0   │                  │
```

The listener's own poll loop classifies each `poll_message` outcome as
`NoWork` (202), `Migrated` (BrokerMigration), `Job` (RunnerJobRequest),
`NetworkError`, or `Cancelled`, backing off 1s → 60s on `NetworkError`.
It runs exactly one job, then returns, and `run_cmd::RunLoop` dispatches
`listener::loop_decision::next_action` over four already-observed facts —
the cancel token, `--once`, `.pending_remove`, and the listener's
`Result` (precedence: cancel > `--once` > `.pending_remove` > outcome):

- **cancelled** (SIGINT/SIGTERM) → exit 0.
- **`--once`** → exit with the listener's status (legacy single-job).
- **`.pending_remove`** (a `remove` ran while the lock was held) → exit 0.
- **`Ok(())`** (job done) or **`Err(Auth)`** (the single-use JIT is
  dead) → **re-mint** a fresh config and re-poll.
- any other **`Err`** (transient) → decorrelated-jitter backoff
  (1s → 60s cap, cancel-aware) and retry the still-valid config.

A re-mint POSTs `generate-jitconfig` again through
`register_cmd::remint_and_persist`, which mints, folds the new config
into the prior one via `config::remint::merge_reminted_config`
(preserving `[services]` / `[cache]` / `[workspace]` / `[shadow]`
verbatim), and rewrites `config.toml` / `credentials.json`
all-or-nothing. `wire::net::register` maps 401/403 → `RunnerError::Auth`
(fatal — guidance names `login` / `--token` / `TOOLU_RUNNER_TOKEN`) and
any other non-2xx → `RunnerError::Network` (transient, backs off); a
missing stored bearer is likewise fatal, and `run` WARNs about it at
startup (unless `--once`) so an unattended runner does not silently stop
after one job. The cancellation token is wired through every
`tokio::select!` — including the backoff sleeps — so SIGINT breaks the
loop promptly.

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
~/.toolu-runner/                    # runner home ($TOOLU_RUNNER_HOME overrides)
├── token-<host>.json               # login-token file fallback (keyring first) — SHARED by all repos
├── _work/                          # per-job workspaces (GitHub-style, shared default)
│   └── <repo>/
│       └── <job-id>/
├── config.toml                     # legacy single-slot registration (read-only
│                                   # fallback; org registrations still land here)
├── .runner_version                 # installed toolu-runner version
└── runners/<owner>/<repo>/         # one dir per repo registration = its data_dir
    ├── config.toml                 # registration + runtime config (0600)
    ├── credentials.json            # (0600)
    ├── .lock                       # per-repo single-job lock file (0600, JSON body)
    ├── .pending_remove             # marker written by `remove` while a run is in flight
    └── _diag/                      # log files, diagnostic dumps
        ├── runner.log              # JSON, secret-masked, daily-rotated
        ├── runner.log.YYYY-MM-DD   # rotated archives
        └── jobs/                   # per-job JSONL event journals (watch TUI)
            └── <ts>-<job-id>.jsonl # newest 50 kept, secret-masked
```

`.lock` body is JSON:

```json
{
  "pid": 12345,
  "started_at": "2026-06-18T10:00:00Z",
  "config_path": "/Users/foo/.toolu-runner/runners/owner/repo/config.toml"
}
```

A second `run` reads the body, prints the PID, and exits 2. A stale
lock (holder PID dead AND mtime > 5 min) is removed and re-acquired
by the next `run`. The watcher is intentionally simple — there is no
forked watcher task; recovery happens at the next acquire attempt.

## Release pipeline

Releases run in two halves. The **front half**
(`.github/workflows/release-pr.yml` + `cliff.toml`) turns merged
work into a version bump; the **back half**
(`.github/workflows/release.yml`) turns the resulting tag into published
binaries. The version is no longer hand-edited — git-cliff authors it,
and the only write it makes to `main` is a pull request a human merges.

The front half was originally release-plz; it was replaced by git-cliff
because release-plz's `git_only` mode runs `cargo package` on every
workspace member, which cannot handle this unpublished workspace's
versionless internal path deps (release-plz#2595, fix unmerged).
git-cliff reads git history only.

The front half is gated by the `RELEASE_AUTOMATION_ENABLED` repository
variable (off by default; set it to `true` to activate). Until then both
jobs are inert, so merging to `main` opens no release PR and cuts no tag.

```
merge to main
      │
      ▼
release-pr           computes the next semver with `git-cliff
      │              --bumped-version`, bumps [workspace.package] version,
      │              prepends the new section to CHANGELOG.md from the
      │              conventional commits since the last tag (feat → Added,
      │              fix → Fixed, docs → Documentation, refactor/perf →
      │              Changed; the workspace crates move in lockstep via
      │              version.workspace), and opens/updates the "release" PR.
      ▼
merge the release PR the bump + changelog land on main.
      │
      ▼
release-tag          sees the untagged version on main and pushes the
      │              matching `vX.Y.Z` tag — under RELEASE_PLZ_TOKEN (a
      │              PAT), NOT the default GITHUB_TOKEN. GitHub suppresses
      │              workflow runs for events raised by GITHUB_TOKEN, so a
      │              tag pushed by it would never fire release.yml; the PAT
      │              is a distinct identity, so its push does. That tag
      │              hands off to the back half:
      ▼
```

The back half is unchanged. It reads the repo and never writes to it;
by the time it runs, `Cargo.toml` + `CHANGELOG.md` already carry the
version the release PR wrote.

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
      │              → release notes; gh release create (contents: write,
      │              --prerelease iff the tag contains '-').
      ├──────────────┐
      ▼              ▼
finalize         homebrew        both `uses:` reusable workflows, chained on
(matrix ×4)      (stable only)   `needs: publish` — NOT triggered by the
                                 release event. See below.
```

The asset names + tarball layout are the contract `install.sh`
consumes (`toolu-runner-<os>-<arch>.tar.gz`, binary at root, service
files under `scripts/`). glibc-dynamic only — a static musl build is
deferred because `tokio-tungstenite` pulls `native-tls` → openssl-sys.
The four release scripts are unit-tested against real repo files under
`scripts/test/` and run in `ci.yml`.

Once `publish` has cut the release, two more workflows run inside the
same workflow run, chained on `needs: publish`. Neither gates the
release itself — by the time they run it already exists.

**Why chained, not event-triggered.** Both originally listened for the
release-published event. That can never fire here: GitHub's rule is
that "events triggered by the `GITHUB_TOKEN` will not create a new
workflow run", and `publish` calls `gh release create` with the
default `GITHUB_TOKEN`. A release so created emits no release event,
so both workflows sat dead in the repo. They are now
`on: workflow_call:` reusable workflows invoked from `release.yml`.
The alternative — handing `gh release create` a PAT so the event does
fire — was rejected: it puts a second write-scoped token in the
pipeline to buy back an event we don't need, since `needs:` already
expresses the ordering. Under `workflow_call` the `github` context is
the **caller's**, so `github.ref_name` is the pushed tag and there is
no `github.event.release` payload at all; each workflow's static test
rejects any expression that reads one.

`.github/workflows/release-finalize.yml` (`contents: read`) downloads
each target's tarball + `SHA256SUMS` straight back from the release
(not the build artifact) and verifies the checksum, a size-sanity
floor, and the `tar` member layout — catching upload corruption or a
stale `SHA256SUMS` that `publish` can't see, since `publish` only
checksums what it's about to upload, never what actually lands. It
never edits the release or the repo; a failure here is a signal, not
a rollback. `gh release create` attaches assets synchronously, so
`needs: publish` is sufficient ordering — there is no upload-visibility
race to wait out.

`.github/workflows/release-homebrew.yml` (`contents: read`, skipped
for prerelease tags via a job-level `if:` on `github.ref_name`)
downloads `SHA256SUMS` the same way, renders `Formula/toolu-runner.rb`
via `scripts/generate-homebrew-formula.sh` (an `on_macos`/`on_linux` ×
`on_arm`/`on_intel` formula selecting one of the four release
tarballs), and pushes it to the external `Falconiere/homebrew-tap`
repo using a `HOMEBREW_TAP_TOKEN` fine-grained PAT — the default
`GITHUB_TOKEN` has no access outside this repo. A called workflow is
granted `github.token` automatically but sees no other secret unless
the caller passes it, and `release.yml` passes this one and nothing
else: `secrets: inherit` would forward every repo secret to a workflow
whose job is to push to an external repository. The callee declares it
under `on.workflow_call.secrets` as `required: true`. The prerelease
guard likewise lives in the callee, not the caller, so the tap can
only ever point at a stable release regardless of who calls it. A
no-op push (formula unchanged) is a normal outcome, not a failure.
Missing the PAT fails this workflow only; the GitHub Release is
unaffected either way.

`release.yml` holds a workflow-level `concurrency` group keyed on the
tag (`cancel-in-progress: false`), covering the whole chain — one
release per tag, never cancelled mid-flight.

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

The full failure-mode coverage lives in
`crates/toolu-runner/tests/failure_modes_test.rs` (12 scenarios).

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