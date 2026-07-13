# toolu-runner

Standalone GitHub Actions JIT runner — no orchestrator service and
no OTel.

## Crate Type

- Layered workspace of **10 crates under `crates/`**. `toolu-runner`
  is now a **bin-only** crate (the CLI entrypoint: `main.rs` +
  `cli.rs` + `register_cmd.rs` + `login_cmd.rs` + `status_cmd.rs`).
  The execution **engine** lives in
  `execution`; the GitHub **JIT lifecycle** lives in `listener`.
- Workspace members (dependency order): `protocol`, `shared`,
  `config`, `expressions`, `cache`, `wire`, `observability`,
  `execution`, `listener`, `toolu-runner`.
- Dependency graph (acyclic):
  - `protocol` — no internal deps (sync crypto + protocol/v1 types).
  - `shared` — no internal deps (cross-cutting types, tracing init,
    `SecretMasker`, `sanitize_job_id`, `runner_os`/`runner_arch`).
  - `config`, `expressions`, `cache` — each depend on `shared` only.
  - `wire` — `shared`, `protocol`.
  - `observability` — `shared`, `config`.
  - `execution` — `shared`, `expressions`, `cache`.
  - `listener` — `execution`, `wire`, `observability`, `shared`,
    `protocol`.
  - `toolu-runner` (bin) — `shared`, `protocol`, `config`, `wire`,
    `observability`, `listener` at runtime (and, as dev-deps,
    `execution` / `expressions` / `cache` for the integration tests).

## Crate-Specific Rules

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
  `bollard`, `axum`). `wire::net` owns all async I/O.
- **One-way `protocol` → `wire` boundary.** `protocol`
  exposes `pub fn` builders and parsers; the async `pub async fn`
  HTTP transport lives in `wire::net`. Tests against
  `protocol` need no HTTP client, no clock, no tokio.
- **Per-repo single-job concurrency.** Each registration lives in
  `<home>/runners/<owner>/<repo>/` (home = `$TOOLU_RUNNER_HOME`, else
  `~/.toolu-runner`; the dir is the persisted `data_dir`).
  `toolu-runner run` acquires an exclusive `fs2` file lock on that
  dir's `.lock` (body: JSON with `pid`, `started_at`, `config_path`),
  so jobs for *different* repos run concurrently while a second `run`
  for the same repo reads the body and exits 2 with the holder's PID.
  Legacy single-slot registrations (read-only fallback; also org-level
  registrations) lock `<home>/.lock`. Stale locks (holder PID dead AND
  mtime > 5 min) are removed and re-acquired by the next `run`.
  Caveat: concurrent cross-repo runs in `offline` / `accelerated`
  services mode need a distinct `service_bind` per repo config
  (EADDRINUSE otherwise); default `forwarder` binds nothing.
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
  → docker → composite.
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
  `ACTIONS_RUNTIME_TOKEN` stays the real token. Wired in the
  `execution` crate's `service_endpoints` module.
- **Secret masking:** `shared::SecretMasker` (wrapped by
  `shared::MaskerRedactor`) is registered as the tracing
  `SecretRedactor` so registered `secrets.*` values (and their
  JSON-escaped variants) never reach `_diag/runner.log` unredacted.
- **No daemon mode for `run`.** The CLI blocks until SIGINT / SIGTERM.
  Service files wrap it.
- **No `build_tool_*`** — build-tool modules were cut in the port.
  `service_auth` / `service_lifecycle` are kept (they back the
  OIDC/artifact/cache axum services).

## Key Modules

### `protocol/` — sync, no I/O, no network (no deps)

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

### `shared/` — cross-cutting types + tracing init (no deps)

- `config.rs` — `RunnerConfig` (data_dir, workspace_root, cgroup_path,
  services_mode, service_bind, cache, workspace_gc_hours,
  shadow_enabled) + `ServicesMode` (forwarder / offline / accelerated),
  `CacheConfig`, `L2Config`.
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
  `/var/lib/toolu-runner`) + `sanitize_job_id` (job id →
  filesystem-safe file name; used by the journal).
- `platform.rs` — `runner_os` / `runner_arch` (host OS/arch labels;
  shared by `wire`, `listener`, and `execution::context_build`).
- `secret_masker.rs` — `SecretMasker` (`add_secret` + per-line `mask`)
  and `MaskerRedactor` (implements `startup::SecretRedactor`) so
  registered `secrets.*` values never reach `_diag/runner.log`
  unredacted.
- `startup.rs` — `init` / `init_with_redactor` (tracing init with
  `RUST_LOG` / `TOOLU_RUNNER_LOG` EnvFilter), `SecretRedactor` trait,
  `RedactingMakeWriter` / `RedactingWriter` (line-level secret
  redaction), `.env` loader.

### `config/` — registration config, lock, token store (deps: shared)

- `config.rs` — `RunnerRegistrationConfig`, `RuntimeConfig`,
  `CredentialsFile`, the `[services]` / `[cache]` / `[workspace]` /
  `[shadow]` sections (`ServicesSection`, `CacheSection`,
  `WorkspaceSection`, `ShadowSection`) + their `shared::RunnerConfig`
  mappers, `load_config` / `save_config` (TOML, 0600),
  `load_credentials` / `save_credentials` (JSON, 0600),
  `jit_endpoint_for_host` (github.com → `pipelinesgh.azureedge.net`,
  any other host → `pipelines.<host>`), `resolve_data_dir`,
  `resolve_work_dir`.
- `lockfile.rs` — single-job `fs2` file lock. `acquire(path,
  config_path)` returns a `LockGuard`; `Drop` releases the OS
  advisory lock. Stale-lock recovery uses `is_pid_alive` (sysinfo)
  + mtime > 5 min.
- `auth_store.rs` — GitHub token persistence. `AuthStore`
  (`Keyring` via the `keyring` crate / `File(<data_dir>/token-<host>.json)`
  0600 fallback), `StoredToken`, per-host `save`/`load`/`delete`,
  pure `pick_bearer` (flag > env > stored) + `resolve_bearer`, and the
  pure TTY gate `decide_bearer` → `BearerDecision` (`Use` /
  `StartDeviceFlow` / `Fail` naming `--token` / `TOOLU_RUNNER_TOKEN` /
  `login`). The store is pinned to the runner home (shared by all
  repos). Used only for the `generate-jitconfig` bearer — never at
  runtime.
- `registry.rs` — per-repo registration layout + discovery:
  `runner_home()` (`$TOOLU_RUNNER_HOME`, `~` expanded, >
  `~/.toolu-runner`), `runner_dir` (`<home>/runners/<owner>/<repo>`,
  path-component validation incl. NUL rejection), `RegistrationEntry`,
  `list_registrations` (scan `runners/*/*/config.toml` + legacy root;
  returns `Result` — missing dirs are `Ok(empty)`, an unreadable
  existing dir is an `Err` naming the path, stray non-dir entries are
  skipped), `resolve_config_path` (flag > cwd-inferred > sole
  registration > error listing candidates).
- `repo_infer.rs` — cwd repo inference: pure `parse_remote_url`
  (scp-like / `https://` / `ssh://` remote forms) + `detect_repo`
  (`git -C <cwd> remote get-url origin`; each error names the `--url`
  escape hatch).

### `expressions/` — the `${{ }}` evaluator (deps: shared)

- The full `${{ }}` evaluator: `lexer`, `parser` (AST + precedence +
  primary), `evaluator`, `template`, `functions` (builtins,
  `hashFiles` — registered with the dispatcher; `glob_walk` + `hash`
  back it — plus JSON convert), `context_data`, `types` (`ExprValue`).

### `cache/` — content-addressed CI cache (deps: shared)

- Content-addressed CI cache backing `ServicesMode::Accelerated`:
  `cas/` (content-addressed store — `manifest`, `chunker`, `store`,
  `chunk_io`, `index`, `gc`; FastCDC + BLAKE3), `twirp/` (v2
  `CacheService` RPCs), `blob/` (Azure-blob-compatible endpoint),
  `v1/` (legacy REST on the CAS), `tier/l2.rs` (S3 cold tier),
  `server.rs`, `proxy.rs` (selective reverse proxy), `scope.rs` (read
  ladder + write scope), `trust.rs` (branch-scoped writes),
  `accelerated.rs`.

### `wire/` — async HTTP transport + reporting domain types (deps: shared, protocol)

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
  `connectionData` discovery, timeline record POST), `register`
  (the live JIT `generate-jitconfig` POST).
- `reporting/` — domain types and async wrappers for the Run
  Service and Results Service. `run_service` (request/response
  shapes, `map_conclusion`), `results_service` (Twirp request
  types, signed-URL helpers), `feature_detection` (V1 vs V2
  detection), `live_log` (WebSocket streamer for real-time logs to
  the GH UI), `log_upload`, `results_types`, `types` (`Status`,
  `ReportConclusion`, `StepResult`, `Annotation`).

### `observability/` — job journal + watch TUI (deps: shared, config)

- `journal/` — per-job JSONL event journal, the local observability
  surface behind `watch`. `types` pins the on-disk line contract
  (v1: `{"v":1,"seq":N,"ts":"…","type":"<snake_case event>",…}`,
  decoupled from `shared::events` — internally-tagged serde enum
  flattened into a version/seq/ts envelope). `writer` masks
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
  Missing/unreadable config falls back to multi-dir browsing:
  `discover_jobs_dirs` (every `runners/<owner>/<repo>/_diag/jobs` from
  `config::registry::list_registrations` + the legacy home) merged by
  `scan_all_jobs`, re-discovered on each rescan.
  Test fixture: `tests/fixtures/journal/canonical.jsonl`, captured
  from a real engine run via `JOURNAL_CAPTURE=1 cargo test -p
  toolu-runner --test journal_writer_test capture_canonical`.

### `execution/` — job execution engine (deps: shared, expressions, cache)

- `lib.rs` — `Runner` struct (config, `execute_job` returns an
  `mpsc::Receiver<RunnerEvent>`).
- `execution/` — the engine. `job_runner::run_job` is the single
  entry point. `steps_runner` runs the per-job step loop. `handlers/`
  dispatches by `runs.using`: `script` (shell), `node` / `node_exec`
  (Node.js actions, auto-downloaded), `docker` (bollard), `composite`
  (composite actions), `resolve` (kind detection). `actions/` resolves
  and downloads actions (`resolver`, `downloader`, `manifest`).
  `workflow/` parses workflow YAML (`parser` with `jobs` / `triggers`
  / `raw_types`), `matrix` (build matrix), `orchestrator` (job graph,
  plan), `reusable` (reusable workflow resolution), `trigger`,
  `job_graph`, `types`. `artifacts/` (upload / download via
  Azure append-blob; `backend` + `service` with `handlers` /
  `lifecycle`). `oidc/` (OIDC token server + claims). `context` (env,
  secrets, masking), `composite_*` (composite action scaffolding),
  `step_env` / `step_host` / `step_naming` / `step_state` (step
  helpers), `action_exec` / `action_support` (action invocation
  glue), `cgroup_join` (reserved), `command_parser`, `depth_tracker`,
  `file_commands`, `service_auth` / `service_lifecycle` (back
  OIDC/artifact/cache axum services). E0–E3 wired the live job path:
  `command_dispatch` (stdout `::workflow-command::` pipeline),
  `node_stage` / `post_drain` (pre/post step stages + `STATE_`),
  `composite_uses` (local `./` + composite nested `uses:`),
  `step_timeout` (`timeout-minutes` / `working-directory`), `job_spec`
  / `job_hooks` (job `outputs:`, `defaults.run`, job hook env),
  `context_build` (full `${{ }}` context), `service_endpoints`
  (forwarder / offline / accelerated service-URL injection).
  `workspace_gc.rs` prunes `workspace_root/<job_id>` older than
  `gc_after_hours` (never the running job's). `shadow/`
  (`fingerprint`, `record`) does off-by-default per-`run:`-step
  workspace fingerprinting, appending masked `would_hit` / `false_hit`
  records to `_diag/shadow/<job_id>.jsonl` — records only, never
  serves.
- `docker/` — bollard wrapper. `client` (Docker daemon), `services`
  (service container lifecycle), `path_translator` (host ↔
  container path mapping).
- `node/` — Node.js runtime detection + caching. `runtime` (version
  detection, download, cache at `data_dir/node/{version}`).
- `plugin/` — `RunnerPlugin` trait + `PluginRegistry`. New
  addition not in upstream `actions/runner`.

### `listener/` — GitHub JIT lifecycle (deps: execution, wire, observability, shared, protocol)

- `handler::GitHubListener` is the entry point: parse JIT →
  authenticate (RSA → JWT → OAuth2) → create session →
  `poll_and_execute`. `job_lifecycle::poll_and_execute` owns the
  long-poll loop with exponential backoff (1s → 60s cap), acquire_job,
  run_acquired_job, acknowledge_message, complete_job. `message_route`
  is the pure "what does the runner do with this broker message type"
  decision (unit-testable without a live broker).
  `execution_loop::execute_with_renewal` runs the job with a 60s
  renewal task, an event forwarder that streams logs to the
  Results Service, and a oneshot that captures the final conclusion.
  `setup_step::report_setup_step` reports "Set up job" as step 1
  (matches C# runner order). `step_reporter::StepCollector`
  aggregates per-step results. `helpers::spawn_renewal` is the
  background renewal task. `log_uploader/` owns the per-step log
  streamer and the combined job-log upload. `helpers::cleanup_session`
  deletes the broker session on exit. Listener events are drained to
  the `observability::journal` writer (replacing the old no-op drain).

### `toolu-runner/` — CLI bin (deps: shared, protocol, config, wire, observability, listener)

- `cli.rs` — the clap surface: `Cli` (top-level parser with
  Examples/Environment `after_help` — `TOOLU_RUNNER_TOKEN` /
  `TOOLU_RUNNER_CLIENT_ID` / `TOOLU_RUNNER_HOME` in the Environment
  footer — `propagate_version`, `arg_required_else_help`), `Command`
  enum, per-subcommand args structs with full `--help` text (`--url`
  is `Option` — absent means "infer from the cwd git remote"; every
  `--config` doc states the resolution default), the arg-default
  helpers (`default_labels`, `runner_name_or_hostname`,
  `work_folder_or_default`, `credentials_path_for`), and
  `debug_assert_cli` (clap's definition self-check, run at startup in
  debug builds — exercised by the shell-out CLI tests since the
  bin-only crate has no lib target for a unit test).
- `main.rs` — CLI entrypoint: parse + dispatch (`register` →
  `register_cmd`, `login`/`logout` → `login_cmd`, `status` →
  `status_cmd`) plus the `run` / `remove` / `watch` handlers.
  `--config` resolution for `run` / `remove` / `status` / `watch`:
  flag > cwd-inferred `runners/<owner>/<repo>/config.toml` > the sole
  registration (legacy `<home>/config.toml` included) > error listing
  candidates (`config::registry::resolve_config_path`). `run`
  loads the config + credentials, acquires the per-repo `.lock`
  (`config::lockfile`), constructs `GitHubListener::new(jit_config, …)`,
  wires SIGINT/SIGTERM to a `CancellationToken`. `remove` writes
  `.pending_remove` if `.lock` is held, otherwise deletes
  `config.toml` / `credentials.json` / `.lock` / `.pending_remove` and
  keeps `_diag/` history (live GH unregister call is step 10). `watch`
  opens the journal TUI (`observability::watch`; no network, no
  tracing init — logs would corrupt the alternate screen).
- `register_cmd.rs` — `cmd_register` + `register_and_persist` (split
  out of `main.rs`). `--url` optional: absent infers the repo from the
  cwd git remote `origin` (`config::repo_infer`; github.com only —
  GHES and org runners need an explicit `--url`). Bearer: `--token` >
  `TOOLU_RUNNER_TOKEN` env > stored login token
  (`config::auth_store::resolve_bearer` against the home-root store);
  no token + interactive stderr → inline device flow
  (`auth_store::decide_bearer` + `login_cmd::run_device_flow`),
  non-interactive fails listing the manual options. POSTs
  `generate-jitconfig` (`wire::net::register_jit`), parses the minted
  config, persists config + credentials into
  `<home>/runners/<owner>/<repo>/` (org URLs keep `<home>/config.toml`;
  `--config` overrides) with `data_dir` = the registration dir —
  all-or-nothing, config rollback on a credentials-write failure. Also
  pre-creates the registration dir's `_diag/` (WARN-not-fatal — a
  self-evident layout nicety; `run` recreates what it needs anyway).
- `login_cmd.rs` — `LoginArgs` / `LogoutArgs` (positional host; **no
  `--config`** — the token store is pinned to
  `config::registry::runner_home()`, shared by all repos) + `cmd_login`
  / `cmd_logout` handlers, the shared `run_device_flow` (reused by
  `register`'s inline flow), and the browser-open helper.
  Holds the baked-in `const DEVICE_CLIENT_ID` (placeholder until the
  OAuth App is registered — using it errors before any network call);
  until then every device flow needs `--client-id` (login) or
  `TOOLU_RUNNER_CLIENT_ID` env, and GHES always does.
- `status_cmd.rs` — `cmd_status`: prints the persisted registration,
  credential presence, and any stored device-flow login token for the
  registered host **plus per-host login state**. No network (split
  out of `main.rs`).

## References

- Root `CLAUDE.md` (project-wide rules — when added).
- [docs/architecture.md](docs/architecture.md) — high-level design +
  sequence diagrams for register / run / cancel / reconnect.
- [docs/known-bugs.md](docs/known-bugs.md) — listener bug tracker.
- [docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md](docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md)
  — design spec (gitignored as `docs/toolu/`).
- [docs/toolu/plans/2026-06-18-toolu-runner-standalone.md](docs/toolu/plans/2026-06-18-toolu-runner-standalone.md)
  — build plan.
