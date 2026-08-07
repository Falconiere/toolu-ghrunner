# toolu-runner

Standalone GitHub Actions JIT runner — no orchestrator service and
no OTel.

## Crate Type

- Layered workspace of **10 crates under `crates/`**. `toolu-runner`
  is now a **bin-only** crate (the CLI entrypoint: `main.rs` +
  `cli.rs` + `register_cmd.rs` + `run_cmd.rs` + `boot_cmd.rs` +
  `service_cmd.rs` + `login_cmd.rs` + `status_cmd.rs` +
  `create_app_cmd.rs`).
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

## House Conventions & The Gate

This repo follows [toolu-conventions](https://github.com/Falconiere/toolu-conventions)
(`CORE.md` + `stacks/rust`). Read that kit's `CORE.md` and
`stacks/rust/STRUCTURE.md` first; the rules below are the ones that
bite here.

- **The gate is ONE command**, run before every push and mirrored
  step-for-step by `.github/workflows/ci.yml`:
  `./tools/check.sh all` = `cargo fmt --check` → `cargo clippy
  --workspace --all-targets -D warnings` → the `#[allow]` ban →
  `bash scripts/guardrails/run.sh` → `cargo test --workspace`.
  **No layer may be disabled to get it green** — not for an unrelated
  failure, not "just this once".
- **`scripts/guardrails/` is copied VERBATIM from the kit** and is
  never hand-edited — change `guardrails.config.json` instead. It
  needs `jq` **and** `ast-grep` on PATH (`cargo install ast-grep
  --locked`) and exits 3 without either, so a missing dependency can
  never look like a clean run.
- **Workspace guardrail layout.** The root carries
  `guardrails.workspace.json` (the manifest naming all 10 crates) and
  must **NOT** carry a `guardrails.config.json`; each crate carries
  its own. `run.sh` re-execs once per crate with the working directory
  inside it, so every path in a crate config is **crate-relative**.
- **No `mod.rs` barrels.** A module that outgrows one file becomes
  `src/foo.rs` (declaring `mod bar;`) **beside** `src/foo/bar.rs`.
  There are zero `mod.rs` files in this repo and the gate keeps it
  that way.
- **Tests never share a file with production logic.** No inline
  `#[cfg(test)] mod tests { … }` bodies — the test code lives in a
  sibling `tests/` folder and the production file keeps only the
  one-line include:
  ```rust
  #[cfg(test)]
  #[path = "tests/parse_config.rs"]
  mod tests;
  ```
  Each `tests/` tree stays **flat** (a directory inside a `tests/`
  dir fails the folder-tree check). Crate-root integration tests stay
  in the top-level `tests/`, one file per surface. Real data, no
  mock-data tests.
- **Every `src/` submodule folder carries a `README.md`** listing what
  belongs there, what does not, and one row per file. `src.requireReadme`
  in each crate's config enforces it — when you add a module folder,
  add the README and the config entry together.
- **Doc line on every public item and module** — `///` on every `pub`
  fn, struct, enum, trait, variant and field, `//!` at the top of each
  module. `missing_docs = "warn"` under `-D warnings` makes an
  undocumented public item a CI failure.
- **Env reads go through the crate's `config` module.** A guardrail
  pattern rule (`no-direct-env-var`) bans `std::env::var` anywhere but
  `config.rs` / `config/**` / `tests/**`, so every variable a crate
  reads is declared and validated in one place. `config`, `shared`,
  `protocol`, `execution` and `toolu-runner` each have a `src/config.rs`
  holding their accessors.
- **500 code-line ceiling per file** (blanks and comment-only lines
  excluded; anything under a `tests/` dir is exempt). Split the design
  before you fight the gate.

### Deliberate deviations from the kit

CORE allows deviating; documenting it here is what makes it a
decision rather than drift.

| Deviation | Kit says | Here | Why |
| --- | --- | --- | --- |
| Cargo workspace | one crate, no workspace | 10 crates under `crates/` | The layering (`protocol`/`shared` → … → `listener`) is what keeps `protocol` provably I/O-free; the kit's workspace guardrail machinery supports it. |
| `rustfmt.toml` `tab_spaces` | `4` | `2` | Adopting 4 would reformat all 341 `.rs` files (4,212 hunks) and reset `git blame` for no behavioural gain. Every other rustfmt option matches the kit. |
| Lint set | `unwrap_used`/`expect_used` = `warn` | `deny`, plus `panic`/`unreachable`/`todo`/`indexing_slicing`/cast lints denied | Stricter than the kit, not looser. A runner that panics mid-job kills a customer's build. |
| `#[allow(…)]` / `#[expect(…)]` | not addressed | banned repo-wide (`tools/check.sh`) | Fix the lint, don't silence it. Note the check matches the outer form only — a crate-level `#![allow(…)]` (as in the two `benches/*.rs`) slips through. |
| `clippy::cast_precision_loss` | on via `pedantic` | `allow` (the other three cast lints stay `deny`) | `PipelineContextData::n` is `f64` because GitHub's context numbers *are* doubles; the `i64`/`u64` widening in `job_message::context_data_de` is the wire contract. No lossless narrowing exists, so denying it would force a suppression at the one honest site. |
| `functionSize.max` | `100` | `150` (matches `clippy.toml` `too-many-lines-threshold`) | One number, one enforcer — the two must agree, and 150 is the ceiling this codebase was written to. |
| `knip` / `jscpd` | absent on Rust by design | absent | Node tools; clippy's `dead_code`/`unused_imports` and cargo cover the ground. The kit says the same. |
| Code-review workflow | `deepseek` + generic checklist | `openrouter` + `.github/code-review-prompt.md`, `RULES_REF: merge`, `MAX_ROUNDS`, `MAX_TOKENS` | Repo-tuned; strictly more configured than the template. |

`secrets.scanExempt` in `crates/config` and `crates/toolu-runner`
lists the **synthetic** credential fixtures (`MIIphony`,
`ghs_EXAMPLE…`, `ghp_deadbeef…`) used to test secret masking. Adding
to that list requires proving the value is not a real credential.

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
  listen to it (the whole always-online loop shares one token, held
  across re-mints). `--once` is the single-job opt-out; the default is
  the re-mint loop (see the always-online run loop rule below).
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
- **Always-online run loop (default).** `run` no longer exits after one
  job. Per iteration `run_cmd::RunLoop` reloads `config.toml`, runs one
  `GitHubListener` lifecycle, then dispatches
  `listener::loop_decision::next_action`: on a completed job — or a
  listener `Auth` error (the single-use JIT is dead) — it re-mints a
  fresh JIT config with the stored `login` / `TOOLU_RUNNER_TOKEN` bearer
  (`register_cmd::remint_and_persist`) and re-polls; on any other
  (transient) error it sleeps a decorrelated-jitter backoff (1s → 60s
  cap, cancel-aware) and retries the still-valid config. Fatal (exit
  non-zero) on a missing or auth-rejected re-mint bearer, naming `login`
  / `--token` (register) / `TOOLU_RUNNER_TOKEN`. Cancel (SIGINT/SIGTERM)
  or a `.pending_remove` seen between jobs → clean exit 0. `--once` opts
  back into single-job semantics (exit with the listener's status). The
  per-repo `.lock` is held for the whole loop.
- **No self-daemonization for `run`.** `run` blocks in the foreground
  and loops; it never double-forks. Boot/crash persistence is delegated
  to the OS: `toolu-runner install-service` generates and activates a
  launchd LaunchAgent (macOS, label `io.toolu.runner.<owner>.<repo>`,
  `launchctl bootstrap gui/<uid>` fallback `load -w`) or a systemd user
  unit (Linux, `toolu-runner-<owner>-<repo>.service`, `Restart=always`,
  `systemctl --user enable --now`) wrapping `run --config <path>` — the
  supervisor owns process lifetime, the loop owns registration lifetime.
  No daemon code. (The static single-slot `scripts/` units installed by
  `install.sh --service` still exist for legacy hosts.)
- **`boot`: zero-config one-shot container mode.** No flags — env-driven
  (`TOOLU_JITCONFIG` required, `TOOLU_DEADLINE` optional watchdog
  backstop, epoch-ms) for the toolu.sh provider image's ENTRYPOINT.
  Builds an in-memory `RunnerConfig` (no `config.toml`, no
  `credentials.json`, no `.lock`, no auth store, no re-mint) and runs
  `GitHubListener::run` once. `boot_cmd::cmd_boot` returns an explicit
  process exit code that `main` applies directly (bypassing the other
  subcommands' uniform Ok-is-0/Err-is-2 mapping): `0`
  Success/Skipped/no-job, `1` Failure/Cancelled or a listener error, `2`
  env/config error before polling, `124` the deadline watchdog fired
  (checked before any network call for an already-past deadline, or
  mid-job via a `CancellationToken` graceful-cancel-then-hard-exit).
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
- `app_manifest.rs` — GitHub **App Manifest flow** helpers (github.com
  onboarding, backs `create-app`): `AppManifest::for_runner` (prefilled
  manifest — `administration:write`, `public=false`, no webhook) +
  `to_json`, `ConversionResponse` + `parse_conversion` (the
  `app-manifests/{code}/conversions` reply), `new_state` (CSRF nonce),
  `form_html` (auto-submitting POST page), `parse_callback_path`
  (redirect query → code, CSRF-checked).

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
  unredacted. `mask` runs one `aho_corasick::AhoCorasick`
  (`MatchKind::Standard`) overlapping pass, merging overlapping/adjacent
  matches into a single `***` — not a `String::replace` per registered
  pattern. The automaton is rebuilt once at the end of `add_secret`;
  falls back to the old longest-first `patterns` replace loop if it
  ever fails to build (WARN-logged) rather than skipping masking.
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
  (`File(<runner home>/token-<host>.json)` 0600 DEFAULT / `Keyring` via
  the `keyring` crate opt-in), `StoredToken`, per-host
  `save`/`load`/`delete`, pure `pick_bearer` (flag > env > stored) +
  `resolve_bearer`, and the pure TTY gate `decide_bearer` →
  `BearerDecision` (`Use` / `StartDeviceFlow` / `Fail` naming `--token` /
  `TOOLU_RUNNER_TOKEN` / `login`). `AuthStore::new` picks the backend:
  `File` by default (macOS Keychain ACLs bind to the code signature —
  every rebuild re-prompts; 0600 matches the on-disk `credentials.json`
  threat model); `TOOLU_RUNNER_KEYRING` (pure `keyring_opted_in`) opts in
  to the OS keyring, gated on the read-only `keyring_reachable` probe;
  `TOOLU_RUNNER_NO_KEYRING` (pure `no_keyring_forced`, back-compat)
  overrides the opt-in and skips the probe (headless/CI/locked
  keyrings). The store is pinned to
  the runner home (shared by all repos). Used for the
  `generate-jitconfig` bearer — at `register` time and on every re-mint
  in the always-online `run` loop.
- `app_store.rs` — GitHub App credential persistence: `StoredApp`
  (`save_app` / `load_app` at `<home>/github-app.json`, 0600) +
  `install_url` / `safe_summary`. Home-root store shared by all repos;
  backs `create-app`. Not yet consumed at runtime (installation-token
  minting deferred).
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
- `remint.rs` — re-mint merge for the always-online `run` loop: pure
  `merge_reminted_config(prior, jit_config, runner_id, client_id)`
  clones `prior` and overwrites ONLY the three mint-derived fields
  (`runner_id`, `auth_token` = client_id, `runtime.jit_config`),
  leaving `[services]` / `[cache]` / `[workspace]` / `[shadow]` (and the
  rest of `[runtime]`) byte-identical. Backs
  `register_cmd::remint_and_persist`.
- `service_unit.rs` — pure supervisor-unit builders for
  `install-service`: `ServiceSpec` + `launchd_plist` (macOS LaunchAgent —
  `KeepAlive` + `RunAtLoad`, XML-escaped, stdout/stderr to
  `<data_dir>/_diag/service.{out,err}.log`) / `systemd_unit` (Linux user
  unit — `Restart=always`, `RestartSec=5`, `WantedBy=default.target`,
  double-quoted `ExecStart` so paths with spaces survive). No I/O — the
  bin writes and activates the rendered text.

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
  `accelerated.rs`. `chunk_io::Durability` (`Fsync` / `Deferred`) gates
  the per-chunk fsync on `write_atomic`: chunk writes default to
  `Deferred` (`CasStore::with_fsync_chunks` / `[cache] fsync_chunks`,
  default `false`; manifest writes are always `Fsync`). `store.rs`'s
  `read_chunk_or_restore` self-heals a BLAKE3 verify-on-read mismatch —
  removes the corrupt chunk (WARN-and-continue on a failed removal)
  and falls through to an L2 restore or a clean miss, instead of
  leaving the entry permanently poisoned.

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
  (the live JIT `generate-jitconfig` POST; non-2xx mapping is
  loop-critical — 401/403 → `RunnerError::Auth` (fatal), any other
  status → `RunnerError::Network` so the re-mint loop backs off on a
  transient 5xx instead of dying), `app_manifest` (the
  `create-app` loopback `CallbackServer` on `127.0.0.1:0` +
  `convert_manifest_code` — POSTs `app-manifests/{code}/conversions`).
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
- `wizard/` — the PURE full-screen setup-wizard state machine (sibling
  to `watch/`, drives `toolu-runner setup`). `state` (`WizardState`
  reducer + `StepEvent` + `probe_skips` — the idempotent
  already-done detection), `input` (key → `Action`), `verify`
  (`verify_decision` — the `_diag/runner.log` online-marker gate),
  `term` (alt-screen writers), `ui` (render). No I/O, no async — the
  impure driver lives in the bin (`setup_cmd` / `wizard_steps`).
  Unit-tested from `crates/toolu-runner/tests/wizard_*_test.rs`.

### `execution/` — job execution engine (deps: shared, expressions, cache)

- `lib.rs` — `Runner` struct (config, `execute_job` returns an
  `mpsc::Receiver<RunnerEvent>`). **The event stream is unmasked** —
  `RunnerEvent::Log` lines carry no masking; every consumer (job/step
  log upload, the live-log WebSocket, the journal writer, the tracing
  file sink) must mask its own line before it reaches a durable sink.
- `execution/` — the engine. `job_runner::run_job` is the single
  entry point, returning a `JobTeardown` (`job_teardown.rs`, `pub mod`
  in `execution/mod.rs`) instead of `()`: it stops local cache servers
  and emits `JobCompleted` before returning, and the caller must drop
  the job's event sender — closing the channel that reports completion
  to GitHub — before calling `JobTeardown::finish`, which runs cache GC
  and joins the `spawn_blocking` workspace sweep so both overlap the
  completion round-trip instead of blocking it. `steps_runner` runs
  the per-job step loop. `handlers/`
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
  background renewal task — fixed 60s interval, no backoff — and now
  also hosts the mid-job outage watchdog (see `outage.rs` below):
  `RunnerError::Network`-class renewal failures feed it, a trip cancels
  the job's `CancellationToken` and sets a write-once flag that
  `execute_with_renewal` reads (after joining the renewal task) to
  override a non-`Success` conclusion to `Failure` plus a "lost
  connection" annotation. `outage.rs` is the pure `OutageWatchdog`
  (300s threshold, latch; `pub`, tested from `crates/listener/tests/`).
  `retry.rs` (`pub(crate)`) has `retry_transient` — retries
  `RunnerError::Network` only with jittered 1s→60s backoff, cancel-aware,
  budget-bounded (`REPORT_RETRY_MAX` 10 min) — wrapping `complete_job` in
  `job_lifecycle::poll_and_execute`, which also demotes
  `acknowledge_message` to a single-attempt, WARN-only, non-gating call.
  `src/tests/watchdog_retry.rs` + `src/tests/watchdog_trip.rs` (declared
  from `lib.rs` with `#[cfg(test)] #[path = …]`) are the integration test
  home for both. `log_uploader/` owns the per-step log
  streamer and the combined job-log upload. `helpers::cleanup_session`
  deletes the broker session on exit. Listener events are drained to
  the `observability::journal` writer (replacing the old no-op drain).
  `loop_decision::next_action` (→ `LoopAction`) is the pure
  per-iteration decision for the bin's always-online `run` loop —
  precedence cancel > `--once` > `.pending_remove` > outcome (`Ok(())`
  and `Err(Auth)` → `Reregister` since the JIT is spent/dead, any other
  `Err` → `BackoffRetry`) — unit-testable without a broker, mirroring
  `message_route`.

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
  `register_cmd`, `run` → `run_cmd`, `boot` → `boot_cmd` (intercepted
  before the generic dispatch — see `boot_cmd.rs` below), `install-service`
  → `service_cmd`, `login`/`logout` → `login_cmd`, `status` →
  `status_cmd`, `create-app` → `create_app_cmd`, `setup` → `setup_cmd`)
  plus the inline `remove` / `watch` handlers.
  `--config` resolution for `run` / `remove` / `status` / `watch` /
  `install-service`: flag > cwd-inferred
  `runners/<owner>/<repo>/config.toml` > the sole registration (legacy
  `<home>/config.toml` included) > error listing candidates
  (`config::registry::resolve_config_path`). `remove` **acquires** the job
  lock (`config::lockfile::acquire`) to decide whether a run is in flight —
  atomic, reuses the stale-lock rule, and held across the DELETE so no `run`
  can start mid-removal. Lock held + no `--force` → `.pending_remove` +
  exit 2; lock held + `--force` → local state goes but `.lock` is KEPT (fs2
  is inode-scoped; unlinking it would let a second `run` race the live job)
  and the unregister is skipped. Otherwise it unregisters on GitHub
  (`wire::net::unregister_runner` — DELETE `…/actions/runners/{id}`, name
  lookup when no id; a 404 is confirmed by a lookup before counting as
  "already gone", since GitHub also 404s what a token may not see) and only
  then deletes `config.toml` / `credentials.json` / `.lock` /
  `.pending_remove`, keeping `_diag/`. Unregister-first is load-bearing: the
  persisted id/URL are the only handle, so a failed call aborts with local
  state intact. Bearer `--token` > `TOOLU_RUNNER_TOKEN` > stored `login`
  (empty = absent); NO token is a hard error, not a warning —
  `--skip-unregister` is the explicit opt-out. `watch`
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
  `remint_and_persist` (loop path) mints a fresh JIT config, folds it
  into the prior config via `config::remint::merge_reminted_config`
  (preserving `[services]`/`[cache]`/`[workspace]`/`[shadow]` verbatim),
  and rewrites `config.toml` / `credentials.json` all-or-nothing.
- `run_cmd.rs` — the always-online `run` loop (`cmd_run`, extracted from
  `main.rs`). Startup: init tracing, resolve + load config, acquire the
  per-repo `.lock` (held for the whole loop), bridge SIGINT/SIGTERM to
  one `CancellationToken`, and WARN (unless `--once`) when no login token
  is stored. `RunLoop::drive` reloads `config.toml` each iteration, runs
  one `GitHubListener` lifecycle, and dispatches
  `listener::loop_decision::next_action`: re-mint via
  `register_cmd::remint_and_persist` on job-complete / listener-`Auth`;
  decorrelated-jitter backoff (`BACKOFF_START` 1s → `BACKOFF_MAX` 60s,
  cancel-aware `sleep_or_cancel`) on a transient failure; exit on cancel
  / `--once` / `.pending_remove`. A missing or auth-rejected re-mint
  bearer is fatal (`REMINT_TOKEN_HELP` names `login` / `--token` /
  `TOOLU_RUNNER_TOKEN`).
- `boot_cmd.rs` — `cmd_boot`: the zero-config one-shot container mode
  (`toolu-runner boot`, no flags). Reads `TOOLU_JITCONFIG` /
  `TOOLU_DEADLINE` straight from the env, builds an in-memory
  `RunnerConfig` (no persisted state), bridges SIGINT/SIGTERM to a
  `CancellationToken` (reuses `run_cmd::spawn_signal_bridge`), registers
  the raw `TOOLU_JITCONFIG` blob with the `SecretMasker` behind the
  tracing redactor, spawns a deadline watchdog task when `TOOLU_DEADLINE`
  parses, and runs `GitHubListener::run` once — then aborts and joins the
  watchdog handle (so its grace-period sleep can't hard-exit a finished
  process, and a panic in it is logged). Returns an explicit process exit code
  (`main` applies it directly, bypassing every other subcommand's
  uniform Ok-is-0/Err-is-2 mapping) — see the crate-rules bullet above
  for the code table. `parse_deadline_ms` and the exit-code mapping
  are pure; pinned by the shell-out tests in
  `tests/boot_cmd_test.rs` / `tests/boot_cmd_e2e_test.rs` (bin-only
  crate, no lib target for inline unit tests).
- `service_cmd.rs` — `cmd_install_service`: resolve the config like
  `run`, derive the service identity from the registration dir
  (`io.toolu.runner.<owner>.<repo>` / `toolu-runner-<owner>-<repo>.service`;
  legacy root → `io.toolu.runner` / `toolu-runner.service`), render the
  unit via `config::service_unit`, then per the flags: `--print` (stdout
  only) / `--no-activate` (write + print the activation command) /
  default (write + activate) / `--remove` (deactivate + delete,
  idempotent). Activation shell-outs: launchd `launchctl bootstrap
  gui/<uid>` (fallback `load -w`) / `bootout` (fallback `unload`);
  systemd `systemctl --user daemon-reload` + `enable --now` /
  `disable --now`. Files land at `~/Library/LaunchAgents/<label>.plist`
  / `~/.config/systemd/user/<unit>` (honoring `$HOME`). Non-macOS/Linux
  errors naming launchd/systemd. No network, no tracing init.
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
  credential presence, the token-store backend (file default / keyring
  opt-in), and any stored device-flow login token for the registered host
  **plus per-host login state** (the not-logged-in line carries the
  keyring-migration hint). No network (split out of `main.rs`).
- `create_app_cmd.rs` — `cmd_create_app`: runs GitHub's **App Manifest
  flow** to create a user-owned GitHub App in one click (github.com
  only; a `--host` other than github.com errors as unsupported). Binds a
  loopback `CallbackServer` (`wire::net::app_manifest`) on `127.0.0.1:0`,
  opens the browser to a prefilled manifest form
  (`protocol::app_manifest`; `--no-browser` prints the URL instead),
  catches the redirect, exchanges the code at
  `POST app-manifests/{code}/conversions`, and persists the app
  credentials to `<home>/github-app.json` (`config::app_store`, 0600).
  PRINTS the install URL — does NOT install the app or mint installation
  tokens (deferred; `register` still uses `--token`/env/device-flow this
  release). Flags: `--name`, `--host`, `--no-browser`, `--force`.
- `setup_cmd.rs` — `cmd_setup`: the `setup` wizard entrypoint (the
  full-screen ratatui onboarding flow, github.com only). Non-TTY guard
  (clean error naming `login` / `register` / `install-service` — GHES /
  org use the manual commands), `TerminalGuard` RAII terminal restore,
  and the impure async ratatui render loop draining a `StepEvent`
  channel into `observability::wizard::state`. No daemon; delegates the
  step work to `wizard_steps`.
- `wizard_steps.rs` — async step executors (`run_pipeline`: auth →
  register → install-service → verify). Only **auth** (stored login token)
  and **register** (existing registration) pre-skip; **install** is
  idempotently re-applied (re-writes + re-activates the unit, never skipped)
  and **verify** always runs. Reuses the existing cmd cores
  (`login_cmd::run_device_flow`, `register_cmd::register_and_persist`,
  `service_cmd::install_service_core`).
  Bearer precedence `--token` > `TOOLU_RUNNER_TOKEN` > stored login token
  > device flow; verify tails `_diag/runner.log` for the listener's
  `"session created — long-polling for jobs"` online marker.

## References

- [toolu-conventions](https://github.com/Falconiere/toolu-conventions)
  — the house kit this repo follows. `CORE.md` (stack-agnostic rules),
  `stacks/rust/STRUCTURE.md` (the Rust layout), `stacks/rust/SETUP.md`
  (what a scaffold ships), `guardrails/README.md` (the check module in
  `scripts/guardrails/`). See "House Conventions & The Gate" above for
  what applies here and the deviations on record.
- [docs/architecture.md](docs/architecture.md) — high-level design +
  sequence diagrams for register / run / cancel / reconnect.
- [docs/known-bugs.md](docs/known-bugs.md) — listener bug tracker.
- [docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md](docs/toolu/specs/2026-06-18-toolu-runner-standalone-design.md)
  — design spec (gitignored as `docs/toolu/`).
- [docs/toolu/plans/2026-06-18-toolu-runner-standalone.md](docs/toolu/plans/2026-06-18-toolu-runner-standalone.md)
  — build plan.
- [docs/toolu/specs/2026-07-14-always-online-run-loop-design.md](docs/toolu/specs/2026-07-14-always-online-run-loop-design.md)
  — always-online `run` loop + `install-service` design spec.
- [docs/toolu/plans/2026-07-14-always-online-run-loop.md](docs/toolu/plans/2026-07-14-always-online-run-loop.md)
  — always-online `run` loop build plan.
