# toolu-runner

Standalone GitHub Actions JIT runner — no orchestrator service, no OTel.

## Workspace (12 crates, `members = ["crates/*"]`)

`toolu-runner` is **bin-only** (CLI entrypoint: `main.rs` + `cli.rs` +
`register/run/boot/service/login/status/create_app_cmd.rs` (one `*_cmd.rs`
each) + `register_auth.rs` + `app_auth.rs` + `app_install.rs` + `app_import.rs` +
`supervisor.rs`/`supervisor_signal.rs`/`machine_service.rs` (#208) +
`ephemeral.rs` + `network_setup.rs` + `check_*.rs` + `doctor_*.rs`).
Engine = `execution`;
JIT lifecycle = `listener`. Plus standalone `daemon` bin (`toolu-daemon`,
no internal deps, `reqwest` in tests only).

Dependency order: `protocol`, `shared`, `network`, `config`, `expressions`,
`cache`, `wire`, `observability`, `execution`, `listener`, `toolu-runner`.
Acyclic graph: `protocol`→`shared` only; `shared`→∅;
`network`→`shared` (the ONLY production `reqwest` ctor, `client_builder`,
so `protocol` stays `reqwest`-free, #94); `config`, `expressions`→`shared`;
`cache`→`shared` (`network` dev-dep only — `L2Tier` takes a caller-built
client); `observability`→`shared` (never `config`); `wire`→`shared`,
`protocol`, `network`; `execution`→`shared`, `expressions`, `cache`,
`network`; `listener`→`execution`, `wire`, `observability`, `shared`,
`protocol` (live-log client comes from `wire`); `toolu-runner`→`shared`,
`protocol`, `network`, `config`, `wire`, `observability`, `listener`
(+ `execution`/`expressions`/`cache` as dev-deps for integration tests).

## Gate & house rules

Follows [toolu-conventions](https://github.com/Falconiere/toolu-conventions)
(`CORE.md` + `stacks/rust/STRUCTURE.md` first; below is what bites here).

- **Gate = `./tools/check.sh all`**, mirrored by `.github/workflows/ci.yml`:
  `cargo fmt --check` → `cargo clippy --workspace --all-targets -D warnings` →
  `#[allow]` ban → `bash scripts/guardrails/run.sh` → `cargo nextest run
  --workspace` → `cargo test --workspace --doc` (nextest skips doctests;
  needs `cargo-nextest`, CI uses `taiki-e/install-action`). **Never disable a
  layer to go green.**
- **`scripts/guardrails/` is VERBATIM from the kit** — change
  `guardrails.config.json`, never the scripts. Needs `jq` **and** `ast-grep`
  (`cargo install ast-grep --locked`); exits 3 without either.
- Root carries `guardrails.workspace.json` only, **never**
  `guardrails.config.json`; each crate carries its own. `run.sh` re-execs per
  crate, so config paths are **crate-relative**.
- **No `mod.rs` barrels**: `src/foo.rs` (declares `mod bar;`) beside
  `src/foo/bar.rs`.
- **Unit tests never share a file with prod logic**: sibling `tests/` dir +
  one-line include (`#[cfg(test)] #[path = "tests/x.rs"] mod tests;`).
  `tests/` trees stay **flat**; crate-root integration tests live in top-level
  `tests/`, one file per surface. Real data, no mocks.
- **Integration targets** (`autotests = false` + `tests/main.rs`, #108/#109;
  `cache` keeps Cargo's default single file): `toolu-runner` has
  `integration` (`main.rs` declares every `tests/*.rs`) + `process_env`
  (`auth_store_test`, `secret_sink_coverage_test` — mutate env/subscriber);
  `execution` adds `process_env` for `network_step_env_test` (process-wide
  proxy env around job runs). `config`, `daemon`, `listener`, `network`,
  `protocol`, `shared`, `wire` use one `integration` target. New
  `tests/foo.rs` needs `mod foo;` in `main.rs`. Run with
  `cargo test -p <crate> --test integration -- <file>::[name]` (filters are
  OR'd substrings; `toolu-runner` examples prefixed `foo::`). Helpers declared
  once in `main.rs` (`use crate::<helper>;`). Child-binary reruns pass the
  module-qualified `<file>::<test>` to `--exact` (empty match still exits 0).
  `network/tests/helpers/*`, `execution/tests/helpers/orphan_replay.rs`
  declared in the using root (`main.rs` or `process_env.rs`).
- **Every `src/` submodule dir has `README.md`** (belongs / doesn't + row per
  file); `src.requireReadme` enforces it — add README + config entry together.
- **`///` on every `pub` item/field/variant, `//!` per module**
  (`missing_docs = "warn"` + `-D warnings` = CI failure).
- **Env reads only via the crate's `config` module** (`no-direct-env-var`
  bans `std::env::var` outside `config.rs` / `config/**` / `tests/**`);
  every env-reading crate has `src/config.rs`.
- **500 code lines/file** (blanks/comments excluded; `tests/` exempt).

### Deliberate deviations from the kit

CORE allows deviating; recording it here makes it a decision, not drift.

| Deviation | Kit says | Here | Why |
| --- | --- | --- | --- |
| Cargo workspace | one crate | 12 crates | Layering keeps `protocol` provably I/O-free; kit workspace guardrails support it. |
| `tab_spaces` | `4` | `2` | 4 would reformat 341 files (4,212 hunks) for no gain; all other rustfmt opts match. |
| Lint set | `unwrap_used`/`expect_used` warn | `deny` + `panic`/`unreachable`/`todo`/`indexing_slicing`/cast denied | Stricter, not looser — a mid-job panic kills a customer build. |
| `#[allow]` / `#[expect]` | — | banned repo-wide (`tools/check.sh`) | Fix the lint, don't silence it. Outer-form match only; crate-level `#![allow]` (two `benches/*.rs`) slips through. |
| `cast_precision_loss` | on (pedantic) | `allow`, other three cast lints stay `deny` | `PipelineContextData::n` is `f64` (GitHub numbers are doubles); the `i64`/`u64` widening in `job_message::context_data_de` is the wire contract. |
| `functionSize.max` | `100` | `150` (= `clippy.toml` `too-many-lines-threshold`) | One number, one enforcer. |
| `knip` / `jscpd` | absent on Rust | absent | Covered by clippy `dead_code`/`unused_imports` + cargo. |
| Code-review workflow | `deepseek` + generic checklist | `openrouter` + `.github/code-review-prompt.md`, `JEV_ENABLED`; `review` skipped for same-repo `release-pr` only (fork branch of that name still reviewed) | Release PR is 100% git-cliff/`sed`; review only produced false positives. `scripts/test/code_review_workflow_test.sh` pins skip + fork guard. |

`unwrap_used`/`expect_used` ban panicking `.unwrap()`/`.expect()` only —
`.unwrap_or[_else/_default]()` are permitted and preferred over equivalent
matches. `secrets.scanExempt` in `crates/config` + `crates/toolu-runner`
lists **synthetic** masking fixtures (`MIIphony`, `ghs_EXAMPLE…`,
`ghp_deadbeef…`); extending it requires proving the value isn't a real
credential. `crates/protocol` exempts `tests/fixtures/app_jwt_test_key.pem`:
a real 2048-bit key from `openssl genrsa -traditional 2048` for the #201
App-JWT tests, never uploaded to any GitHub App (authenticates nothing).

## Runtime contracts

- **No OTel in v1**: `tracing-subscriber` + `EnvFilter` only (`info` unless
  `TOOLU_RUNNER_ALLOW_VERBOSE=1`). JSON to `~/.toolu-runner/_diag/<service>.log`
  (daily rotation, ignores `TOOLU_RUNNER_HOME`), pretty to stderr, redacted
  per *Secret masking*.
- **`protocol` is sync, no I/O/network**: deps limited to `serde`,
  `serde_json`, `base64`, `jsonwebtoken`, `num-bigint-dig`, `pkcs1`, `sha1`,
  `sha2`, `aes`, `cbc`, `rsa` (RSA-OAEP AES-key unwrap), `uuid` (+ dev-dep
  `rand`); enforced by `protocol/Cargo.toml` (no `reqwest`/`tokio`/`opendal`/
  `bollard`/`axum`). One-way `protocol`→`wire`: `protocol` holds `pub fn`
  builders/parsers, async transport lives in `wire::net`; `protocol` tests
  need no client/clock/tokio.
- **Per-registration single-job concurrency**: registration at
  `<home>/runners/<owner>/<repo>/` or, for an org URL, `<home>/orgs/<org>/`
  (#208; `$TOOLU_RUNNER_HOME` else `~/.toolu-runner` = persisted `data_dir`). `run` holds an exclusive `fs2`
  `.lock` there for the whole loop (body JSON: `pid`, `started_at`,
  `config_path`); a second `run` for the same repo reads the body and exits 2.
  The legacy single-slot `<home>/config.toml` (never written by default
  now) locks `<home>/.lock`. Stale locks (holder dead AND mtime > 5 min) are
  re-acquired; a released `LockGuard` empties the body (`doctor`/`status`
  read an empty body as stopped — under `run --all` the pid outlives one
  registration's lock). Local `offline`/`accelerated` services bind
  `<service_bind>:0`, so registrations sharing one `service_bind` never
  clash (`cache/tests/server_bind_test.rs`).
- **Stable workspaces, default (#90)**: each `owner/repo` job runs in
  `<work>/<repo>/<repo>` (`runner.workspace` = `<work>/<repo>`), tracked by
  `<work>/_PipelineMapping/<owner>/<repo>/PipelineFolder.json`. The run `.lock`
  does NOT cover it (`boot` has none): each job holds an `fs2` lease on the
  mapping's `.lock` (fresh: `_PipelineMapping/_jobs/<job_id>.lock`) from before
  context build until `run_job` returns; busy lease → fresh `<work>/<job_id>`.
  Dirs are created only after container/services/env evaluation
  (`PreparedWorkspace::create_dirs`). In job containers `RUNNER_WORKSPACE` /
  `runner.workspace` = `/github` (parent of the `/github/workspace` mount).
  `mode = "fresh"` keeps the pre-#90 per-job layout; harnesses pre-populating
  `<work>/<job_id>` pin `WorkspaceMode::Fresh`.
- **Cancellation**: `run` bridges SIGINT/SIGTERM to one
  `tokio_util::sync::CancellationToken` shared by poll loop, renewal task and
  job across re-mints; `--once` opts out to single-job.
- **JIT protocol**: `v2` github.com, `v1` GHES, auto-selected at `register`;
  `wire` `feature_detection` handles the shape diff.
- **Handler dispatch**: no plugin layer, no local orchestrator (removed #97).
  `steps_runner` splits `run:` vs `uses:`; `action_exec` matches
  `runs.using` — node → docker → composite. Unsupported `runs.using` fails at
  manifest parse (`actions/manifest.rs`).
- **Service mode** (`[services] mode`, `ServicesMode` in `shared`):
  `forwarder` (default) injects the REAL GitHub service URLs + runtime token
  from the job message `SystemVssConnection` into step env
  (`ACTIONS_RESULTS_URL`, `ACTIONS_RUNTIME_URL`(+`_TOKEN`), `ACTIONS_CACHE_URL`,
  `ACTIONS_CACHE_SERVICE_V2`, `ACTIONS_ID_TOKEN_REQUEST_URL`(+`_TOKEN`));
  `offline` hosts local fakes (airgap); `Accelerated` serves both cache
  protocols (v2 Twirp via `RESULTS_URL`, v1 REST via `CACHE_URL`) from local
  NVMe while proxying `ArtifactService` + rest to GitHub (real token kept).
  Wired in `execution::service_endpoints`.
- **Secret masking**: `shared::SecretMasker` (+ `MaskerRedactor`) registered
  as the tracing `SecretRedactor`; `secrets.*` values and JSON-escaped
  variants never reach `_diag/runner.log`.
- **Always-online `run` loop (default)**: per iteration `RunLoop::drive`
  reloads `config.toml`, runs one `GitHubListener` lifecycle, then
  `listener::loop_decision::next_action`: completed job or `Auth` error (JIT
  spent) → `remint_and_persist` (stored `login`/`TOOLU_RUNNER_TOKEN`; for an
  `[auth] kind = "app"` registration (#201) a GitHub App installation token
  from `app_auth::InstallationTokens`, memory-cached ≤ 50 min) and
  re-poll; other errors → decorrelated-jitter backoff (1s→60s, cancel-aware)
  on the still-valid config; missing/auth-rejected re-mint bearer → fatal
  naming `login`/`--token`/`TOOLU_RUNNER_TOKEN`/`register --app`
  (`REMINT_TOKEN_HELP`), or for an App its fix (`app_remint_help`: install
  URL, `register --app --replace`) — `Config` from the App path (a key that no
  longer signs) is fatal too; unless `--once`/ephemeral, an App registration's
  App file is loaded, masked and test-signed at start (unusable → exit 2
  before polling); cancel or `.pending_remove`
  between jobs → exit 0; `SessionConflict` (409 outlasting the listener's
  30s×8 retry) → exit `5` (`SESSION_CONFLICT_EXIT_CODE`), never backed
  off/re-minted, even under `--once`. **Ephemeral (#92)**: `ephemeral = true`
  never re-mints — `next_ephemeral_action` decides (job ran, or acquired
  before erroring per `job_acquired()`, → `Unregister`, latter exits 1;
  cancel/`--once`/`.pending_remove` → exit, state kept; conflict → 5;
  `Auth` → 1; else backoff on same JIT); `ephemeral.rs` unregisters + deletes
  local state after the one job.
- **No self-daemonization**: `run` blocks in foreground; persistence is the
  OS supervisor. `install-service` (no `--config`) installs ONE machine unit
  — launchd `sh.toolu.runner` / systemd user `toolu-runner.service`
  (`ExecReload` = SIGHUP, no `RestartPreventExitStatus`) — running
  `run --all`, after deactivating+deleting older toolu units that run a
  registration of this home, a missing config, or another `run --all`
  (units running a config outside the home are kept + warned; the
  `/etc/systemd/system` static unit is report-only, #203). `--config <path>`
  keeps the per-registration unit (`Restart=always` +
  `RestartPreventExitStatus=5`; legacy root → `toolu-runner-legacy.service`,
  orgs → `toolu-runner-orgs-<org>.service`); launchd `KeepAlive` can't
  condition on exit code.
- **`run --all` supervisor (#208)**: holds `<home>/supervisor.lock` (live
  holder → exit 2); one `RunLoop` task per registration
  (`RunLoop::for_registration`, shared with `run`); LockHeld retried every
  5 s; SIGHUP rescans (start new, drain `.pending_remove`/vanished config via
  `GitHubListener::with_drain` — idle only, never an acquired job — restart
  ended loops); a loop's `Err` stops only it and is written masked to
  `<data_dir>/.stopped` (`config::stop_marker`, shown by `status`/`doctor`);
  exit 0 on SIGINT/SIGTERM, never a registration's code; runs ephemeral
  registrations. `register` and `remove` SIGHUP it
  (`supervisor_signal::notify`: lock held + `holder_alive`); `remove` of a
  supervised registration waits ≤30 s (`TOOLU_RUNNER_REMOVE_WAIT_MS`) for
  the drain, exits 2 if a job keeps it busy, and removing the last
  registration uninstalls the machine unit.
- **`boot`: zero-config one-shot container mode** (no flags; ENTRYPOINT for
  the toolu.sh image). Env: `TOOLU_JITCONFIG` (required),
  `TOOLU_DEADLINE` (optional epoch-ms watchdog). In-memory `RunnerConfig`,
  no files/lock/auth-store/re-mint, one `GitHubListener::run`. Exit codes
  (`cmd_boot`, applied directly): `0` Success/Skipped/no-job, `1`
  Failure/Cancelled/listener error, `2` env/config error, `5` session
  conflict, `124` deadline fired (pre-flight past-deadline check or mid-job
  graceful-cancel-then-hard-exit).
- **No `build_tool_*`** (cut); `service_auth`/`service_lifecycle` remain
  (back the OIDC/artifact/cache axum services).

## Key modules (per-crate entry points; full file maps live in each `src/**/README.md`)

- **`protocol/`** (sync): `auth` (PKCS#1 DER → RSA, PS256 JWT
  sub/iss=clientId, aud=url, jti, nbf/iat −30s, exp +4m30s; `parse_rsa_private_key`,
  `build_jwt`, `AccessToken`), `jit_config`
  (3-blob base64 envelope), `runner_version::COMPATIBILITY_VERSION` (pinned
  official-runner identity; product version separate, see
  `docs/runner-updates.md`), `session` (ephemeral `00000000-…` request:
  `CreateSessionRequest`/`Response`, `AgentInfo`, `EncryptionKey`,
  `TaskAgentSession`, `build_session_request`),
  `messages` (`decrypt_message_body` AES-256-CBC + `strip_bom`;
  `rsa_oaep`/`body_decrypt` unwrap encrypted session keys), `types`
  (`.runner`/`.credentials`/`_rsaparams` blobs: `RunnerSettings`,
  `CredentialData`, `RsaKeyParams`), `v1/` (GHES types + pure `resolve_service_url`),
  `app_manifest` (#202 `ManifestTarget` `Personal` | `Org(org)` →
  `form_action` (`settings/apps/new` vs `organizations/{org}/settings/apps/new`)
  + `default_permissions` (`administration:write` vs
  `organization_self_hosted_runners:write`); `public=false`, no webhook;
  `AppManifest::for_target`, `ConversionResponse` (`owner.id` optional) +
  `parse_conversion`, CSRF `new_state`, `form_html`,
  `parse_callback_path` — backs `create-app`), `app_jwt` (#201: RS256
  `build_app_jwt[_at]`, `iat` −60s / `exp` +540s / `iss` = app id, key errors
  never echo the PEM; `Installation` (+ `repository_selection`),
  `InstallationToken` with redacted `Debug`; #202 `AppInfo`/`AppOwner`
  (`GET /app`), `OrgInfo` (`GET /orgs/{org}`)).
- **`shared/`**: `config` (`RunnerConfig` incl. `node_dir`
  `TOOLU_RUNNER_NODE_DIR`, `NetworkConfig` pure data with `Debug` proxy as
  `<set>`, `ServicesMode`, `WorkspaceConfig`, `CacheConfig`/`L2Config`);
  `error::RunnerError`; `events` (`RunnerEvent`, `ListenerEvent`,
  `Conclusion`); `job_message/`; `paths` (`expand_tilde`: HOME →
  USERPROFILE → `/var/lib/toolu-runner`; `sanitize_job_id`);
  `platform` (`runner_os`/`runner_arch` in GitHub spelling shared by
  `wire`/`listener`/`context_build` — but `AcquireJobRequest.runner_os` sends
  RAW lowercase `std::env::consts::OS`, a live contract, don't converge);
  `secret_masker` (single-pass Aho-Corasick, overlapping matches merge to one
  `***`, `Cow` borrowed on no-match, batch `add_secrets` rebuilds once, WARN
  fallback to longest-first loop); `startup` (tracing init, `SecretRedactor`,
  redacting writer, `.env` loader).
- **`network/` (#94)**: `client_builder(&NetworkConfig)` — only prod
  `reqwest` ctor (pinned by `network_client_inventory_test`); env mode keeps
  system proxy, config mode installs `Proxy::all` + percent-decoded Basic auth
  + `NoProxy` (disables env vars); `ca_bundle` must be absolute regular file
  with ≥1 valid anchor else `Config` error naming path; `@`-in-path/query or
  unencoded `/ ? #` in password rejected; `describe` (redacted one-liner) +
  `proxy_configured` share the validation. `proxy_env::ProxyEnv` is the ONE
  proxy-env reader (hyper-util `Matcher` semantics; `parse_url`/`redact_url`/
  `url_secrets` fail safe); `step_env` splits host/container vars
  (`NODE_EXTRA_CA_CERTS` host-only); `secrets::proxy_secrets` drops usernames.
  All env reads in `config.rs`; fixtures `tests/helpers/{tls,proxy}_fixtures.rs`
  `#[path]`-included per test root.
- **`config/`**: `config.rs` (registration/runtime/credentials types, four
  `[services]/[cache]/[workspace]/[shadow]` sections + `shared` mappers,
  TOML/JSON load+save at 0600, `jit_endpoint_for_host` (github.com →
  `pipelinesgh.azureedge.net`, else `pipelines.<host>`),
  `resolve_data/work_dir`);
  `network.rs` (`[network]` section, `deny_unknown_fields`, omitted-when-empty,
  preserved by re-mint/replace; `resolve()` trims `no_proxy`, expands `~`);
  `labels.rs` (#92: `default_labels` = `self-hosted` + `RUNNER_OS`/`RUNNER_ARCH`
  in `shared::platform` spelling, `resolve_labels` additive unless
  `--no-default-labels`; pinned by `tests/fixtures/jit_labels_probe.json`);
  `lockfile.rs` (`fs2` guard, body emptied on drop,
  sysinfo + mtime>5min stale rule); `stop_marker.rs` (`<data_dir>/.stopped`
  0600 JSON `{stopped_at, error}`: why `run --all` stopped a loop); `auth_store.rs` (home-pinned
  `token-<host>.json` 0600 default / `keyring` opt-in via
  `TOOLU_RUNNER_KEYRING`, `TOOLU_RUNNER_NO_KEYRING` back-compat override,
  `pick_bearer` / `resolve_bearer` / `decide_bearer` with `BearerDecision`
  (`Use` / `StartDeviceFlow` / `Fail` naming `--token` /
  `TOOLU_RUNNER_TOKEN` / `login`; pure `keyring_opted_in` /
  `no_keyring_forced`, `keyring_reachable` gate, `StoredToken`);
  `app_store.rs` (`StoredApp` at `<home>/github-app.json` 0600:
  `save_app`/`load_app`/`load_app_file`, `install_url` (`…/installations/new/permissions?target_id=<owner_id>`
  when known), `safe_summary`, `secret_values` for the masker, redacted
  `Debug`; optional #202 `owner_id` / `installation_id` (omitted when unset,
  pre-#202 files load); written by `create-app`, read by `register --app`
  and the App re-mint);
  `registration_auth.rs` (#201 `[auth]`: `AuthSection` tagged on `kind`,
  `Token` default and omitted on save (pre-#201 shape) or
  `App { app_file, installation_id }`, `deny_unknown_fields`);
  `registry.rs` (`runners/<o>/<r>` + `orgs/<org>` layout, `runner_home`/
  `runner_home_is_custom`/`runner_dir`/`org_dir` incl. NUL rejection,
  `RegistrationScope {Repo, Org, Legacy}` + `label()`,
  `list_registrations` (repos, orgs, legacy) returning `Ok(empty)` on missing dirs /
  `Err` naming unreadable ones, `resolve_config_path`: flag > cwd-inferred >
  sole > error); `repo_infer.rs` (`parse_remote_url` + `detect_repo` via
  `git remote get-url` origin, errors name `--url`); `remint.rs` (`merge_reminted_config`, pure merge overwriting ONLY
  `runner_id`/`auth_token`/`runtime.jit_config`, so `[network]`/`[auth]` survive); `service_unit.rs` (pure
  `ServiceSpec` → `launchd_plist` (incl. `WorkingDirectory`,
  `EnvironmentVariables`, `launchd_env_path` PATH fold) / `systemd_unit`
  (incl. `RestartPreventExitStatus=5`); `MachineSpec` →
  `launchd_machine_plist` / `systemd_machine_unit` (`run --all`,
  `ExecReload` SIGHUP, `TOOLU_RUNNER_HOME` pinned only when custom);
  `parse_systemd_target` / `parse_launchd_target` → `Supervises {All,
  Config}` read a unit back for migration).
- **`expressions/`**: `${{ }}` evaluator — `lexer`/`parser`/`evaluator`/
  `template`/`functions` (incl. `hashFiles` via `glob_walk`+`hash`, JSON
  convert)/`context_data`/`types`/`display` (upstream `ToDisplayString` →
  `display_string`, `references` for `CheckHasRequiredContext`, `first_line`).
- **`cache/`** (backs `Accelerated`): `cas/` (FastCDC+BLAKE3: manifest,
  chunker, store, `chunk_io` with `Durability::Fsync`/`Deferred` defaulting
  `Deferred` via `[cache] fsync_chunks`, index, gc; `read_chunk_or_restore`
  self-heals corrupt chunks via L2/miss), `twirp/` v2 RPCs, `blob/`
  Azure-compatible endpoint, `v1/` legacy REST, `tier/l2.rs` (S3 cold tier,
  caller-supplied client, not production-wired), `server`/`proxy`/`scope`/
  `trust`/`accelerated`.
- **`wire/`**: `net/` async transport — `auth` (deleted-registration 4xx
  `invalid_client`, live capture `oauth_invalid_client_400.json`, is `Auth`
  not retried `Protocol`), `device_auth` (host-relative device flow backs
  `login`), `session` (create/delete; 409 → `SessionConflict`), `messages`
  (poll+ack), `run_service` (acquire/renew/complete), `results_service` (Twirp
  step/log RPCs + signed blob URLs), `log_upload` (append-blob),
  `v1` (GHES discovery + timeline), `register` (`generate-jitconfig` + runner
  DELETE under `repos/{o}/{r}` or, for a one-segment URL, `orgs/{org}`
  (`RunnerScope`/`resolve_target`, #201); 429/5xx→`Network` so re-mint backs
  off, other non-2xx→`Auth` fatal), `github_app` (#201 App-JWT calls:
  `find_installation` (`Ok(None)` on 404 for a known App, `Auth` on 404
  `Integration not found`), `list_installations`, `create_installation_token`,
  pure `reuse_deadline`; #202 `get_app` (proves a key), unauthenticated
  `find_org` (`Ok(None)` on 404), `installation_repo_count`; same
  429/5xx→`Network` rule; live 401/404 captures (+ #202 `orgs_200/404`,
  `app_401_bad_jwt`) in `toolu-runner/tests/fixtures/github_app/`),
  `runner_groups` (org groups +
  `pick_group`),
  `app_manifest` (loopback `CallbackServer` 127.0.0.1:0 serving the
  target's form + `convert_manifest_code` POST under the API base).
  `reporting/`: `run_service` shapes + `map_conclusion`, `results_service`
  types/URL helpers (`create_job/step_logs_metadata`), `feature_detection` V1/V2, `live_log` (HTTP/1.1 upgrade
  on caller client; `live_log_client(&NetworkConfig)` is `http1_only`/30s, bad
  `[network]` WARN-disables it), `log_upload`, `results_types`/`types`.
- **`observability/`**: `journal/` JSONL per-job journal, on-disk v1 line
  contract decoupled from `shared::events`; `writer` masks every line, buffers
  256 pre-acquire events, names `<ts>-<job_id>.jsonl` under
  `data_dir/_diag/jobs/`, prunes to 50, never fails the job; `reader` tails +
  `scan_jobs`. Takes `data_dir` from caller, never resolves config. Fixture
  `tests/fixtures/journal/canonical.jsonl` via `JOURNAL_CAPTURE=1`
  (`journal_reader_test`/`journal_writer_test`).
- **`execution/`** (engine): `Runner::execute_job` returns
  `mpsc::Receiver<RunnerEvent>` — **stream is unmasked**, every sink masks its
  own line. `job_runner::run_job` returns `JobTeardown`: stops cache servers +
  emits `JobCompleted`, caller drops sender (closing completion channel)
  before `finish()` runs GC + workspace sweep overlapping the round-trip.
  `handlers/` by `runs.using` (script/node+node_exec auto-downloaded/docker/
  composite/`resolve`); `actions/` resolve+download with single-flight
  `prefetch` (cap 4, top-level + nested join it, failure evicts and never fails
  the job); tarball extraction streams (`bytes_stream`→`StreamReader`→
  `SyncIoBridge`→`spawn_blocking`, never buffered); `artifacts/` (append-blob
  backend/service); `oidc/`; `context`/composite scaffolding/`step_*`
  helpers/`step_attrs` (timeout-minutes/continue-on-error)/`step_display`
  (two-phase names)/`action_exec`+`support`/`command_parser`/`depth_tracker`/
  `file_commands`/`cgroup_join` (reserved)/`service_auth`+`lifecycle`. Step summaries (#83, below) and
  job outputs (#88, below) included. `job_http` (#94: job + accelerated clients
  from `RunnerConfig.network`, proxy-credential masks, host/container exports);
  `service_endpoints`; `orphan_cleanup` (#89: `RUNNER_TRACKING_ID`,
  pre-`JobCompleted` SIGKILL, 15s, never fails; SIP-blind Apple binaries covered
  via registered process groups); `workspace_{layout,tracking,lease}` (#90) +
  `workspace_gc` (only own `data_dir`, unleased, never the running job);
  `shadow/` (off-by-default fingerprint records, never serves). `docker/`
  (bollard; `DOCKER_HOST` mapping incl. Colima/OrbStack/Podman/Rancher; Linux-only
  actions, remote-pre-failure keeps eligible posts; `Dockerfile` image reuse).
  `node/` (#95: upstream `HandlerFactory`/`NodeUtil` node20→24 `migration`,
  glibc/musl + container `/etc/*release` `platform` probe,
  `ensure_node_runtime`: `TOOLU_RUNNER_NODE_DIR` preseed (upstream `externals/`
  layout) → `data_dir/node/…` cache → download pins 20.20.2/24.21.0, 3 attempts).
- **`listener/`** (JIT lifecycle): `handler::GitHubListener`: parse JIT →
  RSA→JWT→OAuth2 → `retry_session_conflict` (409 = ~3min post-session ghost;
  re-auth + retry 30s×8, cancel→`Ok(None)`) → `poll_and_execute` (idle poll
  in `job_lifecycle/idle.rs`, where `with_drain` ends an idle loop with
  `Ok(None)` but never an acquired job; 1s→60s backoff, `acquire_job`, early broker ack (`acknowledge_message` — death
  mid-job trades redelivery for no cancel-watcher re-see) — run,
  single-attempt WARN-only `acknowledge` demotion,
  `retry_transient`-wrapped `complete_job` for `Network` only, 1s→60s jitter,
  `REPORT_RETRY_MAX` 10min budget). Pure `message_route`/`broker_message`
  (+`broker_refresh` answering `ForceTokenRefresh`) and
  `loop_decision::{next_action → LoopAction, next_ephemeral_action →
  EphemeralAction}` (precedence cancel > `--once` > `.pending_remove` >
  outcome; variants `Reregister` / `BackoffRetry` / `Exit` / `Unregister`).
  `execute_with_renewal`: 60s renewal task (also
  hosts the 300s-latch `OutageWatchdog`; trip cancels token and
  `outage_override` flips non-`Success` to `Failure` + `LOST_CONNECTION_MESSAGE`),
  `tokio::join!` live-log + `setup_step::report_setup_step` ("Set up job" =
  step 1, `change_order` 0 vs queue-seeded 1) before engine start; forwarder masks each `Log` once
  (Cow) across upload/live-log/combined sinks; journal re-masks end-to-end
  (`secret_sink_coverage_test`); `StepReportQueue` batches
  `update_workflow_steps` per flush, WARN-drops (never feeds watchdog), 10s
  `drain()` pre-conclusion;
  finalize-split (per-step drain before conclusion, combined upload overlaps
  `complete_job`; handles joined in `cleanup_session` on Ok+Err). Integration
  tests in `src/tests/{watchdog_retry,watchdog_trip}.rs`. `capture::write_acquired`
  (#103: 0600/0700 under `_diag/captures/`, WARN-never-fail, compiled always
  but called only under `capture` cargo feature; sanitize with
  `scripts/test/sanitize_capture.py`).
- **`toolu-runner/`** (bin): `cli.rs` (clap surface incl. `after_help` env
  footer, `debug_assert_cli` via shell-out tests — no lib target).
  `main.rs`: dispatch (`register`/`run`/`boot` + `install-service` +
  `login`/`logout` + `status` + `create-app` + `check`/`doctor`, each its
  `*_cmd.rs`, plus inline `remove`) + `--config` resolution (flag >
  cwd-inferred > sole > error) + inline `remove` (holds lock across DELETE; held+no-`--force` →
  `.pending_remove`, then a `run --all` holder is SIGHUPed and awaited ≤30 s
  (drained → normal removal; busy → exit 2), any other holder → exit 2;
  held+`--force` → state gone but `.lock` KEPT, unregister skipped, supervisor
  notified; else GitHub DELETE with 404-confirmed-by-lookup, then
  delete state (+`.stopped`) keeping `_diag/`, and the last registration's
  removal uninstalls the machine unit; unregister-first so failure aborts intact;
  bearer `--token` > `TOOLU_RUNNER_TOKEN` > stored `login`, none = hard
  error unless `--skip-unregister`; helpers `remove_bearer`,
  `wire::net::unregister_runner`, `delete_registration_state`).
  `register_cmd` (`--url` optional, cwd-infer github.com only; org URL →
  `orgs/<org>/`, SIGHUPs a running `run --all`; `--labels`
  additive, `--no-default-labels`, `--ephemeral`; `generate-jitconfig` →
  persist all-or-nothing with `data_dir` = registration dir; `remint_and_persist`
  preserves service/cache/workspace/shadow/network/auth byte-identical; labels
  via `config::labels`, #92). `register_auth` (#201 bearer: `--app` >
  `--token` > non-empty `TOOLU_RUNNER_TOKEN` > `github-app.json` whose host is
  the URL host (`pick_app`) > stored `login` > inline device flow when
  interactive, else fail naming options; App → `[auth] kind = "app"`;
  `resolve_runner_group` turns an org `--runner-group` name into its id).
  `app_auth` (#201: `load_app` masks `secret_values`, `register_installation`
  with a not-installed error naming where it is installed + install URL,
  `InstallationTokens` one-entry memory cache, `app_remint_help`; `masked`
  runs `register`'s errors and the fatal re-mint message through the masker,
  since `main` prints them to stderr outside the tracing redactor).
  `network_setup::lifecycle_client` (per-iteration + `boot` env mode;
  `NetworkMemo` logs/registers only on change; bad `[network]` = exit 2).
  `ephemeral::finish_job` (unregister+delete keeping `_diag/`; org URLs via
  `orgs/{org}` (#201); no bearer or unaddressable URL → local-only with "not
  verified" output; retryable error keeps state, exit 2).
  `run_cmd::RunLoop::drive` (`BACKOFF_START`→`BACKOFF_MAX` cancel-aware sleep;
  conflict exits 5 via downcast `CodedExit`, all else 2;
  `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS` overrides 30s wait for
  `run`+`boot`; missing/rejected re-mint bearer fatal via
  `REMINT_TOKEN_HELP`; `RunLoop::for_registration` shared with `run --all`;
  drain counts as cancel). `supervisor` (`run --all`, see Runtime contracts;
  `supervisor_test`/`supervisor_fatal_test`/`supervisor_remove_test`).
  `supervisor_signal` (`supervisor_pid`, `notify`, `reload_hint`,
  `drain_and_wait` for `remove`). `machine_service` (machine identity,
  `installed_units` inventory + `Disposition` shared with doctor's `service`
  row (0 WARN / 1 queried / ≥2 FAIL), `cmd_install_machine`,
  `uninstall_if_last`; `machine_service_test`). `boot_cmd` (pure `parse_deadline_ms` + exit mapping;
  `boot_cmd_test`/`boot_cmd_e2e_test`). `service_cmd` (no `--config` → machine
  unit; else identity from registration dir, `--print`/
  `--no-activate`/default/`--remove` idempotent; launchd `bootstrap
  gui/<uid>` (fallback `load -w`) / `bootout`, systemd `daemon-reload` +
  `enable/disable --now` under `$HOME`; refuses `ephemeral` except `--remove`).
  `login_cmd` (positional host, no `--config`; shared `run_device_flow`;
  placeholder `DEVICE_CLIENT_ID` errors pre-network; `--client-id`/
  `TOOLU_RUNNER_CLIENT_ID` required until the OAuth App exists, always on
  GHES). `status_cmd` (registration + `auth:` token or the App's slug/id/
  installation from its local file + credential + backend + `run:` line
  (lock holder / `.stopped` / not running) + per-host login
  state incl. keyring-migration hint; no network). `check_cmd` (#93 read-only
  preflight: `--url`⊕`--config`, `--timeout` 1–300s def 10; exits 0 all-required-OK
  / 1 required-failed / 2 usage-config; never locks/writes/sessions/credentials;
  `check_endpoints` pure host table incl. ServerUrl overrides + GHES/IP skips,
  `check_probe` one GET (any response but 407 passes; source-chain classified),
  `check_tools` optional git/docker/node, `check_env` proxy+trust render,
  `check_report` masked render; tested by `check_cmd_test`/`check_tools_test`
  against loopback peers). `doctor_cmd` (all registrations, `--config`
  narrows, `--offline` sends nothing; 0 no-FAIL / 1 FAIL / 2 bad path; never
  locks — state via lock body + `holder_alive`; one credentialed
  `GET <api>/user`; `doctor_{report,host,auth,registrations,service}.rs`,
  network reuses `check_cmd::gather`; only a persistent *token* registration
  needs a personal bearer, `[auth] kind = "app"` mints its own and gets an
  `app` row that loads its App file (#201);
  `doctor_*_test`). `create_app_cmd` (#202; github.com only; offline guards
  (host, existing file) → `--org` check `GET /orgs/{org}` (404 → exit 2) →
  loopback callback serving the target's form, `--no-browser` prints URL;
  converts code, saves `<home>/github-app.json` AT ONCE; default name
  `toolu-runner-<org|account|hostname>`; then `wait_and_record`).
  `app_install` (`installation_on` = THE "App + its installation on an
  account" lookup (wait, import, `setup`); `wait_for_installation` polls
  `/app/installations` with a fresh JWT, `Network` warns + continues, line
  commands `o`/`c` (OSC 52) on a terminal stdin, timeout prompt Keep/Open/
  Cancel or unattended `TimedOut`; Ctrl-C in `create_app_cmd::run_wait`;
  exits via `CodedExit` 1 timeout / 130 cancel). `app_import`
  (`--import`/`--app-id`+`--app-key`: `GET /app` proof before any write,
  identity refreshed from it, then lookup or wait). Env hooks in `config.rs`:
  `TOOLU_RUNNER_GITHUB_API_URL` (default `https://api.github.com`),
  `TOOLU_RUNNER_INSTALL_WAIT_MS` (600000), `TOOLU_RUNNER_INSTALL_POLL_MS`
  (5000); tested in-process (`src/tests/app_install.rs`) and through the
  binary (`create_app_test`).

### Step summaries (#83)

`RunnerEvent::StepSummary` = immutable masked doc + backend UUID; listener
re-masks and uploads only when a Results endpoint exists (serial queue drained
pre-completion), else skips. Bodies never touch diagnostic sinks (journal keeps
ID + size only); read/size/upload errors never change the conclusion. Scope in
`docs/step-summaries.md`.

### CompleteJob / StepResult (#88)

`StepResult` mirrors upstream `StepResult.cs` (snake_case, string
status/conclusion, `action_name`/`ref`/`type` from `StepMetadata`, always-sent
annotations) + engine-final "Complete job" row evaluating job outputs then
`actionsEnvironment.url` (no `secrets` context; masker-changed value dropped
with upstream warning). `CompleteJobRequest` echoes `billingOwnerId`, sends
`infrastructureFailureCategory` only for typed `RunnerError::ActionFetch` on a
failed step. Lanes in `docs/test-coverage.md` (#88).

## References

- House kit: `CORE.md`, `stacks/rust/STRUCTURE.md` + `SETUP.md`,
  `guardrails/README.md`. Deviations recorded above.
- [docs/architecture.md](docs/architecture.md) — register/run/cancel/reconnect design + sequences.
- [docs/macos-developer-tools.md](docs/macos-developer-tools.md) — un-stalling macOS builds (per-developer, never scripted).
- [docs/known-bugs.md](docs/known-bugs.md) — listener bug tracker.
- [docs/release-profile.md](docs/release-profile.md) — `[profile.release]` `s`+LTO rationale; pinned by `scripts/test/release_profile_test.sh`.
- [docs/compatibility-matrix.md](docs/compatibility-matrix.md) (+
  [docs/compatibility-evidence.md](docs/compatibility-evidence.md)) — epic #67 matrix
  (from `compatibility-matrix.json`, `compat_matrix_check.py --check`); only an
  evidenced `passed` lane counts.
- `docs/toolu/specs/2026-06-18-*.md` + `docs/toolu/plans/2026-06-18-*.md` — standalone design + build plan (gitignored `docs/toolu/`).
- `docs/toolu/specs/2026-07-14-always-online-run-loop-design.md` + `plans/…` — run loop + `install-service` spec/plan.
