# toolu-runner

Standalone GitHub Actions JIT runner — extracted from
`yamless-runner`, with no yamless code paths, no orchestrator service,
and no OTel.

## Crate Type

- Library + Binary (`toolu-runner`, `toolu_runner` lib + `toolu-runner` bin).
- Internal deps: `shared` (types + tracing init), `protocol` (sync
  protocol types + crypto). No `yamless-*` crate references.
- Workspace members: `shared`, `protocol`, `toolu-runner`.

## Crate-Specific Rules

- **No yamless coupling.** No imports from `yamless-runner`,
  `yamless-shared`, `yamless-auth`. No `YAMLESS_*` env vars read.
  Yamless env vars trigger a `WARN` on startup and are ignored.
  Enforced by `tools/check.sh` and `lefthook.yml`.
- **No OTel in v1.** Tracing is `tracing-subscriber` + `EnvFilter`
  only. JSON-formatted to `data_dir/_diag/<service>.log` (daily
  rotation), pretty-printed to stderr. Secret redaction is wired
  through `shared::startup::SecretRedactor` so secrets never reach
  the file sink unredacted.
- **`protocol` crate is strictly sync, no I/O, no network.** Its
  dependency set is restricted to `serde`, `serde_json`,
  `serde_yaml`, `base64`, `jsonwebtoken`, `num-bigint-dig`, `pkcs1`,
  `sha1`, `sha2`, `aes`, `cbc`, `rsa`, `uuid` (dev-dep `rand`). `rsa`
  was added for message-body decryption (RSA-OAEP AES-key unwrap).
  Enforced by `protocol/Cargo.toml` (no `reqwest`, `tokio`, `opendal`,
  `bollard`, `axum`). `toolu-runner::net` owns all async I/O.
- **One-way `protocol` → `toolu-runner` boundary.** `protocol`
  exposes `pub fn` builders and parsers; the async `pub async fn`
  HTTP transport lives in `toolu-runner::net`. Tests against
  `protocol` need no HTTP client, no clock, no tokio.
- **Single-job concurrency.** `toolu-runner run` acquires an
  exclusive `fs2` file lock on `~/.toolu-runner/.lock` (body: JSON
  with `pid`, `started_at`, `config_path`). A second `run` reads
  the body and exits 2 with the holder's PID. Stale locks (holder
  PID dead AND mtime > 5 min) are removed and re-acquired by the
  next `run`.
- **Cancellation token wiring.** `toolu-runner run` builds a
  `tokio_util::sync::CancellationToken` and bridges SIGINT / SIGTERM
  to it. The poll loop, the renewal task, and the in-flight job all
  listen to it. `--once` exits after the first job completes —
  currently also the default behavior, since a JIT registration is
  single-use.
- **JIT config protocol version:** `v2` for github.com, `v1` for
  GHES. Selected automatically by host at `register` time; the
  `feature_detection` module handles the wire-shape difference.
- **Handler dispatch** (in priority order): plugin → script → node
  → docker → composite. There is **no** `yamless` handler variant —
  it was cut in the port.
- **Service mode (forwarder / offline / accelerated).** Config
  `[services] mode` (`ServicesMode` in `shared/src/config.rs`) selects
  how artifacts / cache / OIDC reach their backends. In `forwarder`
  mode (the default), the runner reads the REAL GitHub service URLs +
  runtime token from the job message's `SystemVssConnection` endpoint
  and injects them into step env (`ACTIONS_RESULTS_URL`,
  `ACTIONS_RUNTIME_URL`, `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_CACHE_URL`,
  `ACTIONS_CACHE_SERVICE_V2`, `ACTIONS_ID_TOKEN_REQUEST_URL` /
  `_TOKEN`), so GitHub-hosted `upload-artifact@v4` / `cache@v4` / OIDC
  talk to real GitHub. In `offline` mode the runner hosts the local
  fake services for airgapped use. In `ServicesMode::Accelerated` the
  runner binds a local content-addressed cache that intercepts BOTH
  GitHub Actions cache protocols (v2 Twirp `CacheService` via
  `ACTIONS_RESULTS_URL`, legacy v1 REST via `ACTIONS_CACHE_URL`) and
  serves them from local NVMe, while a selective reverse proxy forwards
  `ArtifactService` and everything else to real GitHub — the injected
  `ACTIONS_RUNTIME_TOKEN` stays the real token. Wired in
  `execution::service_endpoints`.
- **Secret masking:** `execution::secret_masker::SecretMasker` is
  registered as the tracing `SecretRedactor` so registered
  `secrets.*` values (and their JSON-escaped variants) never reach
  `_diag/runner.log` unredacted.
- **No daemon mode for `run`.** The CLI blocks until SIGINT / SIGTERM.
  Service files wrap it.
- **No `build_tool_*`** — yamless build-tool modules, cut in the port.
  `service_auth` / `service_lifecycle` are kept (they back the
  OIDC/artifact/cache axum services).

## Key Modules

### `shared/` — cross-cutting types + tracing init

- `config.rs` — `RunnerConfig` (data_dir, workspace_root, cgroup_path).
- `error.rs` — `RunnerError` enum (Protocol, Auth, Network, Config,
  StepExecution, ScriptHandler, Expression, Docker, Oidc, Artifact,
  Cache, ReusableWorkflow, Reporting, WorkspaceInit, LockHeld, etc.).
- `events.rs` — `RunnerEvent` (`JobStarted`, `StepStarted`, `Log`,
  `StepCompleted`, `JobCompleted`, `Annotation`, `LogGroup`,
  `StepSkipped`) + `ListenerEvent` (wraps `RunnerEvent` plus
  `SessionCreated`, `JobAcquired`, `LockRenewed`, `ReportedStatus`).
  `Conclusion` (Success / Failure / Cancelled / Skipped).
- `job_message/` — `AgentJobRequestMessage`, `ActionStep`,
  `ActionStepDefinitionReference`, `TaskOrchestrationPlanReference`,
  `JobResources`, `JobEndpoint`, `JobAuthorization`, `VariableValue`,
  `MaskHint`, `TemplateToken`, `WorkspaceOptions`,
  `PipelineContextData`, `DictEntry`.
- `paths.rs` — `expand_tilde` (HOME → USERPROFILE →
  `/var/lib/toolu-runner`).
- `startup.rs` — `init` / `init_with_redactor` (tracing init with
  `RUST_LOG` / `TOOLU_RUNNER_LOG` EnvFilter), `SecretRedactor` trait,
  `RedactingMakeWriter` / `RedactingWriter` (line-level secret
  redaction), `warn_about_yamless_env` (AC #23), `.env` loader.

### `protocol/` — sync, no I/O, no network

- `auth.rs` — `parse_rsa_private_key` (PKCS#1 DER from
  `.NET RSACryptoServiceProvider` params, computes CRT components),
  `build_jwt` (PS256, claims: sub=clientId, iss=clientId,
  aud=authorizationUrl, jti=uuid, nbf=now-30s, iat=now-30s,
  exp=now+4m30s), `AccessToken` (OAuth2 response shape).
- `jit_config.rs` — `JitConfig` (parses the 3-blob base64
  envelope: `.runner` / `.credentials` / `.credentials_rsaparams`).
- `session.rs` — `CreateSessionRequest` / `CreateSessionResponse`,
  `AgentInfo`, `EncryptionKey` (encrypted-or-raw AES key),
  `TaskAgentSession`, `build_session_request` (builds the
  ephemeral `00000000-...` session).
- `messages.rs` — `BrokerMessage`, `RunnerJobRequestBody`,
  `BrokerMigrationBody`, `MessageType` (RunnerJobRequest /
  BrokerMigration), `decrypt_message_body` (AES-256-CBC with
  PKCS#7 strip), `strip_bom`.
- `types.rs` — `RunnerSettings` (`.runner` blob), `CredentialData` /
  `CredentialDataInner` (`.credentials` blob), `RsaKeyParams`
  (`.credentials_rsaparams` blob; base64 big-endian integers).
- `v1/` — `ConnectionData`, `JobEvent`, `LocationServiceData`,
  `ServiceDefinition`, `TimelineRecord` (GHES V1 protocol types).
  `resolve_service_url` (pure URL resolver).

### `toolu-runner/` — lib + bin

- `lib.rs` — `Runner` struct (config, `execute_job` returns an
  `mpsc::Receiver<RunnerEvent>`).
- `main.rs` — clap CLI: `login`, `logout`, `register`, `run`,
  `remove`, `status`, `watch`.
  `--config` defaults to `~/.toolu-runner/config.toml`. `login`
  runs GitHub OAuth **device flow** (`net/device_auth`) and stores
  the resulting token via `auth_store` (OS keyring, 0600-file
  fallback); `logout` deletes it. The device-flow OAuth App
  `client_id` is a baked-in `const DEVICE_CLIENT_ID` (placeholder
  until the app is registered); non-`github.com` hosts require
  `--client-id`. `register` validates `--url`, probes the JIT
  endpoint with a 5s HEAD, computes the protocol version from the
  host, and writes a placeholder config (live flow is step 10); its
  `--token` is now **optional** — the bearer is resolved
  `--token` > `TOOLU_RUNNER_TOKEN` env > stored login token
  (`auth_store::resolve_bearer`). `run` loads the
  config + credentials, acquires `.lock`, constructs
  `GitHubListener::new(jit_config, …)`, wires SIGINT/SIGTERM to a
  `CancellationToken`. `remove` writes `.pending_remove` if
  `.lock` is held, otherwise deletes the persisted state (live
  GH unregister call is step 10). `status` prints the config
  **plus per-host login state** without network. `watch` opens the
  journal TUI (no network, no tracing init — logs would corrupt the
  alternate screen).
- `login_cmd.rs` — `LoginArgs` / `LogoutArgs` + `cmd_login` /
  `cmd_logout` handlers, browser-open helper, and data-dir
  resolution (split out of `main.rs` for the 500-line ceiling).
- `auth_store.rs` — GitHub token persistence. `AuthStore`
  (`Keyring` via the `keyring` crate / `File(<data_dir>/token-<host>.json)`
  0600 fallback), `StoredToken`, per-host `save`/`load`/`delete`,
  pure `pick_bearer` (flag > env > stored) + `resolve_bearer`. Used
  only for the `generate-jitconfig` bearer — never at runtime.
- `config.rs` — `RunnerRegistrationConfig`, `RuntimeConfig`,
  `CredentialsFile`, `load_config` / `save_config` (TOML, 0600),
  `load_credentials` / `save_credentials` (JSON, 0600),
  `jit_endpoint_for_host` (github.com → `pipelinesgh.azureedge.net`,
  any other host → `pipelines.<host>`), `resolve_data_dir`,
  `resolve_work_dir`.
- `lockfile.rs` — single-job `fs2` file lock. `acquire(path,
  config_path)` returns a `LockGuard`; `Drop` releases the OS
  advisory lock. Stale-lock recovery uses `is_pid_alive` (sysinfo)
  + mtime > 5 min.
- `net/` — async network layer. **One-way dependency on `protocol`**
  (request types from `protocol`, response types in `protocol` or
  `reporting`). `auth` (token exchange), `device_auth` (GitHub
  OAuth **device flow** — `request_device_code` / `poll_for_token`
  / pure `parse_poll_response`; host-relative so GHES works;
  backs `toolu-runner login`), `session` (create / delete
  session), `messages` (poll + acknowledge), `run_service`
  (acquire / renew / complete), `results_service` (Twirp RPCs:
  `update_workflow_steps`, `create_job_logs_metadata`,
  `create_step_logs_metadata`, signed blob URLs), `log_upload`
  (Azure append-blob: create / block / commit), `v1` (GHES
  `connectionData` discovery, timeline record POST).
- `listener/` — GitHub JIT lifecycle. `handler::GitHubListener` is
  the entry point: parse JIT → authenticate (RSA → JWT → OAuth2) →
  create session → `poll_and_execute`. `job_lifecycle::poll_and_execute`
  owns the long-poll loop with exponential backoff (1s → 60s cap),
  acquire_job, run_acquired_job, acknowledge_message, complete_job.
  `execution_loop::execute_with_renewal` runs the job with a 60s
  renewal task, an event forwarder that streams logs to the
  Results Service, and a oneshot that captures the final conclusion.
  `setup_step::report_setup_step` reports "Set up job" as step 1
  (matches C# runner order). `step_reporter::StepCollector`
  aggregates per-step results. `helpers::spawn_renewal` is the
  background renewal task. `log_uploader/` owns the per-step log
  streamer and the combined job-log upload. `helpers::cleanup_session`
  deletes the broker session on exit.
- `reporting/` — domain types and async wrappers for the Run
  Service and Results Service. `run_service` (request/response
  shapes, `map_conclusion`), `results_service` (Twirp request
  types, signed-URL helpers), `feature_detection` (V1 vs V2
  detection), `live_log` (WebSocket streamer for real-time logs to
  the GH UI), `log_upload`, `results_types`, `types` (`Status`,
  `ReportConclusion`, `StepResult`, `Annotation`).
- `execution/` — job execution engine. `job_runner::run_job` is the
  single entry point. `steps_runner` runs the per-job step loop.
  `handlers/` dispatches by `runs.using`: `script` (shell), `node`
  / `node_exec` (Node.js actions, auto-downloaded), `docker`
  (bollard), `composite` (composite actions), `resolve` (kind
  detection). `actions/` resolves and downloads actions
  (`resolver`, `downloader`, `manifest`). `expressions/` is the
  full `${{ }}` evaluator: `lexer`, `parser` (AST + precedence +
  primary), `evaluator`, `template`, `functions` (builtins,
  `hashFiles` — now registered with the dispatcher, JSON convert),
  `context_data`. `workflow/` parses
  workflow YAML (`parser` with `jobs` / `triggers` / `raw_types`),
  `matrix` (build matrix), `orchestrator` (job graph, plan),
  `reusable` (reusable workflow resolution), `trigger`,
  `job_graph`, `types`. `artifacts/` (upload / download via
  Azure append-blob; `backend` + `service` with `handlers` /
  `lifecycle`). `cache/` (content-addressed CI cache backing
  `ServicesMode::Accelerated`): `cas/` (content-addressed store —
  `manifest`, `chunker`, `store`, `chunk_io`, `index`, `gc`; FastCDC +
  BLAKE3), `twirp/` (v2 `CacheService` RPCs), `blob/`
  (Azure-blob-compatible endpoint), `v1/` (legacy REST on the CAS),
  `tier/l2.rs` (S3 cold tier), `server.rs`, `proxy.rs` (selective
  reverse proxy), `scope.rs` (read ladder + write scope), `trust.rs`
  (branch-scoped writes), `accelerated.rs`. The old `backend/`,
  `service/`, `key.rs` were removed. `oidc/` (OIDC token server +
  claims). `secret_masker`
  (`SecretMasker` with `add_secret` + per-line `mask`; implements
  `shared::startup::SecretRedactor`). `context` (env, secrets,
  masking), `composite_*` (composite action scaffolding),
  `step_env` / `step_host` / `step_naming` / `step_state` (step
  helpers), `action_exec` / `action_support` (action invocation
  glue), `cgroup_join` (reserved), `command_parser`,
  `depth_tracker`, `failure_category`,
  `file_commands`, `service_auth` / `service_lifecycle`
  (back OIDC/artifact/cache axum services). E0–E3 wired the live
  job path: `command_dispatch` (stdout `::workflow-command::`
  pipeline), `node_stage` / `post_drain` (pre/post step stages +
  `STATE_`), `composite_uses` (local `./` + composite nested `uses:`),
  `step_timeout` (`timeout-minutes` / `working-directory`), `job_spec`
  / `job_hooks` (job `outputs:`, `defaults.run`, job hook env),
  `context_build` (full `${{ }}` context), `service_endpoints`
  (forwarder / offline / accelerated service-URL injection).
  `workspace_gc.rs` prunes `workspace_root/<job_id>` older than
  `gc_after_hours` (never the running job's). `shadow/`
  (`fingerprint`, `record`) does off-by-default per-`run:`-step
  workspace fingerprinting, appending masked `would_hit` / `false_hit`
  records to `_diag/shadow/<job_id>.jsonl` — records only, never
  serves. The live JIT register POST lives in `net/register.rs`
  (`generate-jitconfig`).
- `docker/` — bollard wrapper. `client` (Docker daemon), `services`
  (service container lifecycle), `path_translator` (host ↔
  container path mapping).
- `node/` — Node.js runtime detection + caching. `runtime` (version
  detection, download, cache at `data_dir/node/{version}`).
- `plugin/` — `RunnerPlugin` trait + `PluginRegistry`. New
  addition not in upstream `actions/runner`.
- `journal/` — per-job JSONL event journal, the local observability
  surface behind `watch`. `types` pins the on-disk line contract
  (v1: `{"v":1,"seq":N,"ts":"…","type":"<snake_case event>",…}`,
  decoupled from `shared::events` — internally-tagged serde enum
  flattened into a version/seq/ts envelope). `writer` replaces the
  old no-op `ListenerEvent` drain in `listener/handler.rs`: masks
  every line through the job's `SecretMasker`, buffers pre-acquire
  events (cap 256), names the file `<UTC ts>-<job_id>.jsonl` under
  `data_dir/_diag/jobs/`, prunes to the newest 50, and NEVER fails
  the job (WARN once, keep draining). `reader` is the incremental
  replay/tail reader (`poll()` advances only past complete lines)
  plus `scan_jobs` head/tail-window summaries.
- `watch/` — `toolu-runner watch` ratatui TUI. `state` (pure reducer:
  journal lines → job list / step tree / bounded 10k log ring /
  seq-gap flag), `ui` (rendering), `input` (key → `Action`, cancel
  confirm modal), `mod` (250 ms tick loop, 1 s rescan, terminal
  lifecycle, `send_cancel` = SIGINT to the `.lock` PID, unix only).
  Missing config falls back to `~/.toolu-runner` (history browsing).
  Test fixture: `tests/fixtures/journal/canonical.jsonl`, captured
  from a real engine run via `JOURNAL_CAPTURE=1 cargo test -p
  toolu-runner --test journal_writer_test capture_canonical`.
- `types/` — `RunnerConfig` (re-exported from `shared`).

## References

- Root `CLAUDE.md` (project-wide rules — when added).
- [docs/architecture.md](docs/architecture.md) — high-level design +
  sequence diagrams for register / run / cancel / reconnect.
- [docs/known-bugs.md](docs/known-bugs.md) — listener bug tracker.
- [docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md](docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md)
  — design spec (gitignored as `docs/toolu/`).
- [docs/toolu/plans/2026-06-18-toolu-runner-standalone.md](docs/toolu/plans/2026-06-18-toolu-runner-standalone.md)
  — build plan.
