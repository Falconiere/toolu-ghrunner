# Changelog

All notable changes to toolu-runner are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-06-18

The first release of toolu-runner. A standalone self-hosted GitHub
Actions runner written in Rust, extracted from
`yamless-runner`. Ships the full JIT listener protocol (RSA → JWT →
OAuth2 → broker session → message polling → job execution →
reporting), plus a single-job file-lock, a tracing layer with secret
redaction, and a CLI for register / run / remove / status.

### Added

**Workspace layout**

- 3-crate workspace: `shared`, `protocol`, `toolu-runner` (lib +
  bin). No yamless crates imported; no yamless env vars read.
- Strict dependency direction: `shared` is sync and I/O-free;
  `protocol` is sync, I/O-free, and network-free (no `reqwest`,
  `tokio`, `opendal`, `bollard`, `axum`); `toolu-runner` owns all
  async I/O. Enforced by `protocol/Cargo.toml` and CI.

**Listener (JIT protocol lifecycle)**

- JIT config parsing (`protocol::jit_config`) — decodes the 3-blob
  base64 envelope (`.runner` / `.credentials` /
  `.credentials_rsaparams`) into typed Rust structs.
- RSA key reconstruction (`protocol::auth::parse_rsa_private_key`)
  — PKCS#1 DER from `.NET RSACryptoServiceProvider` params, with
  CRT (exponent1, exponent2, coefficient) computation.
- JWT signing (`protocol::auth::build_jwt`) — PS256 with the
  standard GitHub Actions runner claims
  (sub/iss = clientId, aud = authorizationUrl, nbf/iat = now−30s,
  exp = now+4m30s).
- Token exchange (`toolu-runner::net::auth`) — POST to
  `authorization_url` with the JWT as
  `urn:ietf:params:oauth:client-assertion-type:jwt-bearer`.
- Session lifecycle (`protocol::session` + `net::session`) —
  `build_session_request`, `create_session`, `delete_session`.
- Long-poll message loop (`listener::job_lifecycle`) with
  exponential backoff (1s → 60s cap), BrokerMigration handling,
  cancel-token integration.
- Run Service (`net::run_service` + `reporting::run_service`) —
  `acquire_job` / `renew_job` / `complete_job` (every 60s renewal).
- Results Service Twirp (`net::results_service` +
  `reporting::results_service`) — `update_workflow_steps`,
  `create_job_logs_metadata`, `create_step_logs_metadata`,
  signed blob URLs.
- Log upload (`net::log_upload` + `listener::log_uploader`) —
  per-step log streaming + combined job-level log upload via
  Azure append-blob (create / block / commit).
- Live log WebSocket (`reporting::live_log::LiveLogStreamer`) —
  real-time log streaming to the GitHub Actions UI.
- GHES V1 support (`protocol::v1` + `net::v1`) — `connectionData`
  discovery, timeline record POST, `resolve_service_url` helper.
- V1 vs V2 protocol auto-detection — host `github.com` → V2, any
  other host → V1. Selected at `register` time.

**Execution engine**

- Step dispatch (`execution::handlers`) — script (shell), node
  (Node.js actions, auto-downloaded), docker (bollard), composite
  (composite actions). Plugin handler variant precedes built-ins.
- Expression engine (`execution::expressions`) — full `${{ }}`
  evaluator: lexer, AST parser with precedence + primary,
  evaluator, template, function library (builtins, hashFiles,
  JSON convert).
- Action resolution and download (`execution::actions` —
  `resolver`, `downloader`, `manifest`).
- Workflow parser (`execution::workflow::parser`) — jobs,
  triggers, raw types.
- Build matrix (`execution::workflow::matrix`).
- Job graph + orchestration (`execution::workflow::job_graph`,
  `orchestrator`).
- Reusable workflows (`execution::workflow::reusable`) — `uses:
  org/repo/.github/workflows/x.yml` resolution with output
  propagation.
- Artifacts (`execution::artifacts`) — upload + download via
  Azure append-blob. `backend` + `service` with `handlers` /
  `lifecycle`.
- Cache (`execution::cache`) — local disk + remote layered
  backend. `key`, `trust`, `service` with `handlers` /
  `lifecycle`.
- OIDC token issuance (`execution::oidc`) — local server +
  claims.
- Secret masker (`execution::secret_masker::SecretMasker`) —
  registers secrets from job Variables (`IsSecret=true`) and
  MaskHints, splits on newlines, auto-registers JSON-escaped
  variants. Wired into the tracing layer via
  `shared::startup::SecretRedactor`.

**Runtime**

- Single-job file lock (`lockfile.rs`) — exclusive `fs2` lock on
  `~/.toolu-runner/.lock`. JSON body
  (`pid` / `started_at` / `config_path`). Stale-lock recovery
  via `is_pid_alive` (sysinfo) + mtime > 5 min. Released on
  graceful shutdown and on panic (via `Drop`).
- Tracing init (`shared::startup`) — `tracing-subscriber` +
  `EnvFilter` (TOOLU_RUNNER_LOG → RUST_LOG → info). JSON file
  sink at `data_dir/_diag/<service>.log` (daily-rotated), pretty
  stderr sink. Line-level secret redaction via
  `RedactingMakeWriter` + `RedactingWriter`.
- Cancellation token wiring — `tokio_util::sync::CancellationToken`
  built in `toolu-runner run`, bridged to SIGINT / SIGTERM, and
  listened on by the poll loop, the renewal task, and the
  in-flight job. `--once` triggers a 100ms delayed cancel for
  test mode.

**Docker / Node / Plugin**

- Docker client (`docker::client`) — bollard wrapper for daemon
  connection.
- Service containers (`docker::services`) — service container
  lifecycle.
- Host ↔ container path mapping (`docker::path_translator`).
- Node.js runtime (`node::runtime`) — version detection +
  download + cache at `data_dir/_node/<version>`.
- Plugin system (`plugin::RunnerPlugin` + `PluginRegistry`) — new
  addition not in upstream `actions/runner`.

**CLI (`toolu-runner`)**

- `register` — validates `--url`, probes the JIT endpoint with a
  5s HEAD, computes the protocol version from the host, writes
  `config.toml` (TOML, 0600) + `credentials.json` (JSON, 0600) to
  `~/.toolu-runner/`.
- `run` — loads config + credentials, acquires `.lock`,
  constructs `GitHubListener`, wires SIGINT/SIGTERM, runs until
  cancel.
- `remove` — refuses and writes `.pending_remove` if `.lock` is
  held (unless `--force`); otherwise deletes persisted state.
- `status` — prints config summary without network access.
- Subcommand flags (`register` / `run` / `remove` / `status`) —
  matches `toolu-runner --help` output.

**Service install**

- `scripts/io.toolu-runner.plist` — launchd agent for macOS. Sets
  `TOOLU_RUNNER_LOG=info`, redirects stdout / stderr to
  `_diag/launchd-*.log`.
- `scripts/toolu-runner.service` — systemd unit for Linux. Runs
  as `toolu-runner` user with hardened sandboxing
  (`NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`,
  `ProtectHome`, `MemoryDenyWriteExecute`, …), `Restart=always`,
  logs to the journal under `SyslogIdentifier=toolu-runner`.
- `scripts/test/plist_test.sh` and `scripts/test/systemd_test.sh`
  — smoke checks that the unit files parse.

**Install + tooling**

- `install.sh` — installs from GitHub releases. Detects arch
  (x86_64 / arm64) and OS (darwin / linux), downloads the
  matching release artifact, installs to `/usr/local/bin/`
  (or `--install-dir`), optionally installs the service unit
  with `--service`. `--check` prints the plan without downloading.
- `tools/check.sh` — local quality gate (cargo fmt + clippy +
  file-size ≤ 150 + no-allow + no-unwrap + no-yamless).
- `lefthook.yml` — `pre-commit` runs fmt + clippy + yamless
  coupling check; `pre-push` runs `./tools/check.sh all`.

**Release automation**

- `.github/workflows/release.yml` — tag-driven release automation. On
  a `v*` tag push: asserts the tag matches the `Cargo.toml` version,
  runs the fmt/clippy/test gate, builds on four native runners
  (`darwin` / `linux` × `amd64` / `arm64`), packages one
  `toolu-runner-<os>-<arch>.tar.gz` per target (binary + `scripts/`
  service files), computes `SHA256SUMS`, and publishes a GitHub
  Release with notes from this file's matching section. Tags with a
  `-` publish as prereleases. A tag-keyed `concurrency` group covers
  the whole chain. The workflow never writes to the repo.
- `scripts/assert-version.sh` — asserts a release tag matches the
  `[workspace.package]` version.
- `scripts/package-release.sh` — assembles the per-target tarball in
  the exact layout `install.sh` expects.
- `scripts/changelog-extract.sh` — extracts a version's section from
  this file for the GitHub Release notes.
- `.github/workflows/release-finalize.yml` — post-publish smoke test.
  Downloads each target's tarball + `SHA256SUMS` back off the release
  and verifies the checksum, a size floor, and the `tar` member
  layout, catching upload corruption that `publish` cannot see.
- `.github/workflows/release-homebrew.yml` — publishes
  `Formula/toolu-runner.rb` to
  [`Falconiere/homebrew-tap`](https://github.com/Falconiere/homebrew-tap)
  after a stable release (skipped for prereleases), via
  `scripts/generate-homebrew-formula.sh` and a `HOMEBREW_TAP_TOKEN`
  PAT.
- Both of the above are `on: workflow_call:` reusable workflows,
  chained off `publish` with `needs:` rather than triggered by
  `on: release: [published]`. A release created by a workflow step
  using the default `GITHUB_TOKEN` emits no `release` event, so the
  event-triggered form could never fire. A called workflow is granted
  `github.token` automatically but sees no other secret unless the
  caller passes it; `release.yml` passes `HOMEBREW_TAP_TOKEN` and
  nothing else, rather than using `secrets: inherit` (which would
  forward every repo secret to a workflow that pushes to an external
  repo).
- `.github/actionlint.yaml` — declares the `toolu-runner-v1`
  self-hosted label so `actionlint` validates the `*-live.yml`
  workflows instead of erroring on an unknown runner label.
- `scripts/test/{assert_version,changelog_extract,package_release,release_workflow,release_finalize_workflow,generate_homebrew_formula,release_homebrew_workflow}_test.sh`
  — real-data tests for the release scripts + workflows, run in CI.
  The workflow tests assert the chained shape and reject any
  `github.event.release` expression in a reusable workflow.

**Tests**

- 196 tests across `shared`, `protocol`, `toolu-runner`
  (5 CLI, 12 failure modes, 4 listener smoke, 3 net, 16 storage
  layout, 5 shared config, 3 shared error, 5 shared events, 4
  shared startup-redaction, 15 shared job-message, 3 protocol
  auth, 3 protocol integration).

**Documentation**

- `README.md` — install, register, run, remove, status, service
  install, env vars, config schema, troubleshooting.
- `CLAUDE.md` — module map, crate-specific rules.
- `docs/architecture.md` — design + sequence diagrams for
  register / run / cancel / reconnect.
- `docs/known-bugs.md` — B-001 (5-min cancellation watchdog),
  B-002 (live unregistration), B-003 (live register POST).

### Changed

- Renamed `warn_about_yamless_env` → `warn_about_legacy_env` and the
  user-visible warning from "yamless" to "legacy"; behavior unchanged.
  Detection still triggers on the `YAMLESS_` prefix.
- Kept the `no-yamless-coupling` lefthook check name (it still greps for
  the `yamless` / `YAMLESS_` prefix); logic unchanged.



### Removed

- **yamless-orchestrator WebSocket client** (`serve/`, `ws_client/`,
  `command_handler/`, `connect.rs`, `infra.rs`, `lifecycle.rs`)
  — the runner has no orchestrator service to talk to.
- **yamless-specific step handlers** — `yamless`,
  `yamless_deploy`, `yamless_notify`, `yamless_test_report` (and
  their `HandlerKind::Yamless` dispatch variant).
- **`build_tool_*` modules** — yamless build-tool registry
  (`build_tool_detection`, `build_tool_endpoints`,
  `build_tool_storage`).

- **`yamless-auth` CLI** — device-flow authentication. Replaced
  by GitHub's registration-token flow at `toolu-runner register`.
- **OpenTelemetry / OTLP** — replaced by `tracing-subscriber` +
  `EnvFilter` + JSON file sink.
- **`yamless-shared` workspace dependency** — replaced by the
  local `shared` crate.
- **`YAMLESS_*` env var compatibility** — variables are still
  detected and trigger a `WARN` on startup, but they have no
  effect on runner behavior.

[0.1.0]: #010---2026-06-18