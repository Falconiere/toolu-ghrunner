# Improvements

Multi-dimension audit of `toolu-runner` (2026-06-18). Format:
`IMP-{DIM}-{NNN}` — title — dimension — severity (S/M/L) — effort
(S/M/L) — rationale — suggested fix — references.

## Conventions

- **Ranking:** ROI-first (quick wins up top, big rocks at bottom).
  Each entry keeps `severity` + `effort` as fields so readers can
  re-sort. The "Cross-cutting top 5" table at the top is the
  punch-list the next sprint should burn down.
- **Per-dimension cap:** Top 10 per dimension, ranked. Everything
  else (lower ROI, niche, or "noted but not ranked") goes in
  **Appendix (not ranked)**.
- **Dedupe rule:** same file + line + root cause collapses to one
  IMP. Earliest-seeing dimension is primary; the others are
  cross-referenced.
- **Threshold for inclusion:** concretely actionable with a
  one-line fix sketch. Vibe-level "we should think about X" was
  dropped.
- **No `owner` field:** all TBD. Mirrors `docs/known-bugs.md`
  style for unowned items.
- **Real-data only:** every entry points to a real `file:line`,
  not a hypothetical.

## Hard constraints (any fix must respect)

- No re-wiring of `service_auth` / `service_lifecycle` (they back
  OIDC / artifact / cache axum services — see `IMP-DO-003` for the
  doc lie that pretends otherwise).
- No OTel in v1. `tracing-subscriber` + `EnvFilter` only.
- `protocol` crate stays sync + I/O-free (no `reqwest`, `tokio`,
  `opendal`, `bollard`, `axum`).
- House size limits: 500 lines / Rust file, 300 / TS file.
- Real-data test strategy — no synthetic mock-data findings.

## Out of scope (explicit)

- Upstream parity with `actions/runner` (separate concern).
- Step 10 / live smoke (tracked in `docs/known-bugs.md`).
- v1.0.0 release scope itself.

---

## Cross-cutting top 5 (do these first)

The single IMPs that should be on the next sprint's burn-down,
regardless of dimension. Each is concretely actionable in a
single PR.

| ID | Title | Dim | Sev | Eff | Why-first |
| --- | --- | --- | --- | --- | --- |
| **IMP-SE-001** | SecretMasker never wired to tracing — every secret reaches `_diag/runner.log` | Security | S | S | The CLAUDE.md guarantee "secrets never reach the file sink unredacted" is aspirational, not enforced. `init_with_redactor` is built and tested but never called. |
| **IMP-SE-003** | Path traversal in `LocalBackend::artifact_dir` | Security | S | S | A workflow can write to arbitrary paths under the runner user via `{"Name":"../../tmp/pwn"}`. |
| **IMP-SE-004** | Tar slip in `extract_tarball` | Security | S | S | Action tarballs containing `../` components escape `dest`. Combined with `set_executable_if_needed`, this is RCE on the runner host. |
| **IMP-SE-002** | Step log lines uploaded unredacted to GH | Security | S | S | Once `IMP-SE-001` lands, the masker must run on lines going to the gzipped blob + live-log WebSocket, not just the file sink. |
| **IMP-CQ-002** | Divergent `SystemVssConnection` lookup semantics in same module | Code | S | S | One site is case-sensitive, the other is `eq_ignore_ascii_case`. Live-log vs run-service can pick different endpoints. |

---

## Code quality (top 10)

### IMP-CQ-001 — `map_conclusion` duplicated byte-for-byte in two listener modules
- **Severity:** M | **Effort:** S
- **Rationale:** `helpers.rs:94-101` (pub(super)) and `step_reporter.rs:114-121` (private) define the same `match Conclusion -> ReportConclusion`. New `Conclusion` variants need two edits.
- **Fix:** Delete the private copy; import the pub(super) one. Or move both into a `pub(super) fn` on a small `listener::conclusion` submodule.
- **Refs:** `crates/listener/src/helpers.rs:94`, `crates/listener/src/step_reporter.rs:114`

### IMP-CQ-002 — Divergent `SystemVssConnection` lookup semantics within same module
- **Severity:** S | **Effort:** S
- **Rationale:** `connect_live_log` (line 224-232) is case-sensitive on `e.name == "SystemVssConnection"`; `extract_system_token` (line 244-257) uses `eq_ignore_ascii_case`. Same module, same data, two semantics — one path succeeds where the other returns `None`.
- **Fix:** Extract `fn system_vss_access_token(job_msg: &AgentJobRequestMessage) -> Option<String>` into `listener::helpers`; both sites call it. Pick case-insensitive as the canonical form.
- **Refs:** `crates/listener/src/job_lifecycle.rs:224`, `crates/listener/src/job_lifecycle.rs:244`

### IMP-CQ-003 — Home-directory resolution inlined three times
- **Severity:** M | **Effort:** S
- **Rationale:** `shared::paths::home_dir` is the documented home-resolver but `startup::default_data_dir` and `main::default_config_path` each reimplement the `HOME → USERPROFILE → fallback` cascade. Drift is invisible.
- **Fix:** Have both sites call `shared::paths::expand_tilde(Path::new("~"))`; delete their bodies.
- **Refs:** `crates/shared/src/paths.rs:45`, `crates/shared/src/startup.rs:36`, `crates/toolu-runner/src/main.rs:138`

### IMP-CQ-004 — HTTP error-handling boilerplate duplicated across 6 `net/` files
- **Severity:** M | **Effort:** M
- **Rationale:** `net/{auth,run_service,session,messages,v1,log_upload}.rs` all repeat the same 5-line dance: send → `status().is_success()` → `text().await.unwrap_or_default()` → map to `RunnerError::Protocol`. `net::results_service` already has the right `twirp_post` pattern; the other six pre-date it.
- **Fix:** Extract `pub(crate) async fn check_status(response: reqwest::Response, op: &str) -> Result<reqwest::Response, RunnerError>` in `net/mod.rs`; replace the 6 duplicated blocks.
- **Refs:** `crates/wire/src/net/auth.rs:35`, `crates/wire/src/net/messages.rs:50`, `crates/wire/src/net/run_service.rs:38`, `crates/wire/src/net/session.rs:27`, `crates/wire/src/net/v1.rs:32`, `crates/wire/src/net/log_upload.rs:61`

### IMP-CQ-005 — CLI entry points return `Box<dyn Error>`, dropping `RunnerError` structure
- **Severity:** M | **Effort:** M
- **Rationale:** `main.rs::run` and the four `cmd_*` functions all return `Result<(), Box<dyn std::error::Error>>` and `.map_err(|e| format!("{e}"))` 9 times to bridge from `RunnerError`. The typed variants collapse into flat strings before reaching `main`'s `eprintln!` — losing the variant-level matchability the enum was designed for.
- **Fix:** Change the `cmd_*` return types to `Result<(), RunnerError>`. `RunnerError`'s `thiserror` `#[error("…")]` already produces a clear message.
- **Refs:** `crates/toolu-runner/src/main.rs:129`, `crates/toolu-runner/src/main.rs:214`, `crates/toolu-runner/src/main.rs:297`, `crates/toolu-runner/src/main.rs:394`, `crates/toolu-runner/src/main.rs:439`

### IMP-CQ-006 — Unhandled "docker actions not yet supported" returns `Conclusion::Failure` with no error
- **Severity:** L | **Effort:** S
- **Rationale:** `action_exec.rs:148-150` matches `RunsUsing::Docker`, emits a log line, and returns `Ok(Conclusion::Failure)`. The step reports as a generic failure with no actionable cause. The other unhandled case (local actions) at line 64 correctly returns `RunnerError::ActionResolution`.
- **Fix:** Return `Err(RunnerError::ActionResolution("docker actions not yet supported".into()))` and let `steps_runner` surface the error.
- **Refs:** `crates/execution/src/execution/action_exec.rs:148`, `crates/execution/src/execution/action_exec.rs:63`

### IMP-CQ-007 — 34 pub items ship without doc comments (house rule violation)
- **Severity:** L | **Effort:** M
- **Rationale:** 34 `pub fn/struct/enum/trait/const` items across `shared/protocol/toolu-runner` have no preceding `///` or `#[doc = …]`. Worst offenders: `crates/protocol/src/v1/types.rs` (7 GHES V1 wire types) and `crates/execution/src/execution/context.rs` (6 public methods on the central `ExecutionContext`).
- **Fix:** Add `///` doc lines to each of the 34. For `crates/protocol/src/v1/types.rs` in particular, every struct is the V1 wire contract and deserves a one-line description.
- **Refs:** `crates/protocol/src/v1/types.rs:29`, `crates/execution/src/execution/context.rs:146`, `crates/execution/src/plugin/registry.rs:14`, `crates/execution/src/execution/service_lifecycle.rs:60`

### IMP-CQ-008 — `docker_cache.rs` is pub-but-unreferenced dead code
- **Severity:** L | **Effort:** S
- **Rationale:** `pub DockerCacheConfig`, `pub DockerCacheEvent`, `pub DockerPlatform`, `pub fn docker_layer_key` — none are referenced from any other module (grep-confirmed). Reachable as `pub mod docker_cache` so the symbols are crate-public but inert.
- **Fix:** Wire `DockerCacheConfig` into cache or artifact service, or delete `docker_cache.rs` + remove `pub mod docker_cache` from `execution/mod.rs`.
- **Refs:** `crates/execution/src/execution/docker_cache.rs:8`, `crates/execution/src/execution/docker_cache.rs:53`, `crates/execution/src/execution/mod.rs:19`

### IMP-CQ-009 — Unbounded `Vec<String>` accumulates every job log line in memory
- **Severity:** L | **Effort:** M
- **Rationale:** `listener/execution_loop.rs:135` allocates `let mut all_job_lines: Vec<String> = cfg.setup_lines;` and pushes every `RunnerEvent::Log` line into it (line 170) for the entire job, then gzips the whole `Vec` in memory (`log_uploader/upload.rs:80-88`) before the final upload. A multi-hour verbose job can pin hundreds of MB.
- **Fix:** Stream-append the combined job log to `data_dir/_diag/{job_id}.log` as events arrive, then upload the finished file. Or cap `all_job_lines` with `Vec::with_capacity(1 << 16)` and flush to a temp file when it fills.
- **Refs:** `crates/listener/src/execution_loop.rs:135`, `crates/listener/src/execution_loop.rs:170`, `crates/listener/src/log_uploader/upload.rs:80`

### IMP-CQ-010 — Blocking `std::fs` in async functions across `execution/` and `listener/`
- **Severity:** L | **Effort:** L
- **Rationale:** `job_runner.rs:31,32,89,139,149`; `steps_runner.rs:136`; `composite_exec.rs:28`; `action_support.rs:127`; `file_commands.rs:49,83-87,115`; `actions/downloader.rs:34,63,68,75,93,104`; `composite_env.rs:123-125,136,142,145` all use `std::fs::*` inside `async fn`. Each is a latent thread-pool stall.
- **Fix:** Switch to `tokio::fs::*` (the pattern is already used in `artifacts/backend.rs:88,106,111,124,129,142,155`). Mechanical per call site.
- **Refs:** `crates/execution/src/execution/job_runner.rs:31`, `crates/execution/src/execution/steps_runner.rs:136`, `crates/execution/src/execution/composite_exec.rs:28`

---

## Test gaps (top 10)

### IMP-TG-001 — `parse_action_ref` untested against real ref strings
- **Severity:** S | **Effort:** S
- **Rationale:** `parse_action_ref` (and `resolve_action_refs`) is the only path that turns `actions/checkout@v4` into a tarball URL — every Node/composite action step flows through it. The module has zero tests; existing tests construct `ActionRef`s via `with_ref_type` instead of feeding `uses:` strings through the parser.
- **Fix:** Add a default-lane test driving `parse_action_ref` against real-shape refs: `actions/checkout@v4`, `owner/repo/path/to/action@main`, `owner/repo@<40-char-sha>`, `./.github/actions/local`, plus error cases.
- **Refs:** `crates/execution/src/execution/actions/resolver.rs:57`

### IMP-TG-002 — `extract_tarball` never exercised against real tar bytes
- **Severity:** S | **Effort:** M
- **Rationale:** `extract_tarball` strips GitHub's `{owner}-{repo}-{sha}/` prefix and preserves file modes from the tar header. It is the core of action + node-runtime download. No test drives it with a real `tar.gz` blob. Off-by-one in prefix-stripping or `unix-exec` bit loss would not surface. (See also `IMP-SE-004` for the security framing of the same function.)
- **Fix:** Add a default-lane test that builds a synthetic tarball via the `tar` crate with a `prefix/` top-level directory + nested file (and executable mode bit on Unix), feeds it to `extract_tarball`, and asserts the file lands at `dest/file` with mode 0o755 preserved.
- **Refs:** `crates/execution/src/execution/actions/downloader.rs:33`

### IMP-TG-003 — Handler dispatch untested for plugin + node24 + unknown branches
- **Severity:** M | **Effort:** S
- **Rationale:** `resolve_handler` is documented as `plugin → script → node → docker → composite`, but existing tests only cover empty-registry cases. The plugin override branch, `node12`/`node16`/`node24` classification, and the unknown branch's `HandlerKind::Unknown(_)` payload are not independently verified.
- **Fix:** Add default-lane tests: register a fake plugin named `telepathy` and verify a `telepathy` `uses:` resolves to `HandlerKind::Plugin("telepathy")`; verify `node12` / `node16` / `node24` each map to `HandlerKind::Node`; verify the unknown branch returns the original `using` string.
- **Refs:** `crates/execution/src/execution/handlers/resolve.rs:29`, `crates/toolu-runner/tests/composite_action_test.rs:180`

### IMP-TG-004 — `log_upload` mode selection (BlockBlob vs AppendBlob) untested
- **Severity:** M | **Effort:** S
- **Rationale:** `UploadMode::for_content` and `LogUploader::upload` switch transport at the 4 MiB threshold. The helpers build Azure-specific headers, but the only test that uses them is `secret_masker_real_test::full_pipeline` which never exercises the blob path.
- **Fix:** Add a default-lane test asserting `UploadMode::for_content(0)` returns `BlockBlob`, `for_content(4 * 1024 * 1024)` returns `BlockBlob` (boundary), and `for_content(4 * 1024 * 1024 + 1)` returns `AppendBlob`. Plus a header-shape assertion for each helper.
- **Refs:** `crates/wire/src/reporting/log_upload.rs:19`

### IMP-TG-005 — Topological sort + cycle detection untested
- **Severity:** S | **Effort:** S
- **Rationale:** `topological_sort` prevents the orchestrator from deadlocking on `needs:` cycles. Public, propagates `RunnerError::Expression` on cycle. No test exercises the happy path (4-job linear chain), parallel branches, or the cycle case.
- **Fix:** Add a default-lane test: (a) sort `a → b → c → d` into `[a,b,c,d]`, (b) handle a diamond `a → {b,c} → d` (any valid order), (c) reject a cycle `a → b → a` with an error mentioning `cycle`, (d) sort empty input.
- **Refs:** `crates/execution/src/execution/workflow/job_graph.rs:12`

### IMP-TG-006 — Matrix expansion (include/exclude) untested
- **Severity:** M | **Effort:** M
- **Rationale:** `expand_matrix` produces the Cartesian product of base keys, applies exclude, then merges include entries. No test drives any of: 3-axis matrix, exclude targeting one combo, include overriding a single field, or the empty-config single-empty-combo fallback.
- **Fix:** Add a default-lane test against `MatrixConfig { base: { os: [linux, macos], arch: [x64, arm64] }, exclude: [...], include: [...] }`. Plus an empty `MatrixConfig::default()` case that returns `[{}]`.
- **Refs:** `crates/execution/src/execution/workflow/matrix.rs:8`

### IMP-TG-007 — Lockfile stale-but-alive branch not exercised
- **Severity:** M | **Effort:** S
- **Rationale:** `handle_contended` (lockfile.rs:130) has three branches: holder PID alive → `LockHeld`; holder dead + lock fresh → `LockHeld` (fail-closed); holder dead + lock >5min old → remove + retry. Only the `pid-alive` and `stale-removed` branches have tests.
- **Fix:** Add a default-lane test that writes a `LockBody { pid: 0 (dead), started_at: now }` (fresh mtime) to the lock file, calls `lockfile::acquire`, and asserts `Err(LockHeld { pid: 0, .. })` (fail-closed, no steal).
- **Refs:** `crates/config/src/lockfile.rs:130`, `crates/toolu-runner/tests/failure_modes_test.rs:88`

### IMP-TG-008 — Cancellation token bridging to in-flight step never tested
- **Severity:** M | **Effort:** M
- **Rationale:** `cmd_run` builds a `CancellationToken`, bridges SIGINT/SIGTERM, and (with `--once`) a 100ms delayed cancel. The test only asserts `listener.run()` returns when cancellation fires at the poll loop. There is no test that proves cancellation mid-job propagates into the in-flight `Runner::execute_job`. `B-001` (outage watchdog) hinges on this token being live at every await point.
- **Fix:** Add a `#[tokio::test]` that constructs `GitHubListener::new` with a degenerate JIT config, spawns `listener.run(cancel)`, fires `cancel.cancel()` after 50ms, and asserts the call returns within a tight bound.
- **Refs:** `crates/toolu-runner/src/main.rs:354`, `crates/toolu-runner/tests/listener_smoke_test.rs:68`

### IMP-TG-009 — Workflow command parser (`::set-output` / `::error`) has no test
- **Severity:** M | **Effort:** M
- **Rationale:** `command_parser::parse_command` extracts `::error file=…`, `::set-output name=key::val`, `::add-mask`, `::group`, `::save-state`, `::debug`. It is wired into the runner pipeline (must be — `composite_action_test` uses `::set-output` in its fixture), yet has zero tests. (See also `IMP-SE-006` for the security framing of the same dead code path.)
- **Fix:** Add a default-lane test driving `parse_command` with real-shape lines: `::error file=src/foo.rs,line=10,col=5,title=Bad::msg`, `::set-output name=greeting::hello world`, `::add-mask::secretvalue`, `::group::Title`, `::endgroup`, `::debug::trace`, plus malformed inputs.
- **Refs:** `crates/execution/src/execution/command_parser.rs:62`

### IMP-TG-010 — File-command parser (`GITHUB_ENV/OUTPUT/PATH/STATE`) has no test
- **Severity:** M | **Effort:** M
- **Rationale:** `parse_env_file`, `parse_output_file`, `parse_path_file`, and the `KEY<<DELIM` heredoc branch drive the runner's file-command contract. The 1 MiB summary truncation (`truncate_summary`) is also untested.
- **Fix:** Add a default-lane test parsing real-shape env files: `KEY=value`, heredoc `MULTI<<EOF\nline1\nline2\nEOF`, mixed line endings, `NODE_OPTIONS=--inspect` (must be stripped from results), empty values; plus an oversize summary input to assert `truncate_summary` lands at the UTF-8 char boundary.
- **Refs:** `crates/execution/src/execution/file_commands.rs:126`

---

## Security / hardening (top 10)

### IMP-SE-001 — `SecretMasker` is never wired to the tracing subscriber — every secret reaches `_diag/runner.log`
- **Severity:** S | **Effort:** S
- **Rationale:** `cmd_run` in main.rs:298 calls `startup::init` (no redactor) instead of `startup::init_with_redactor`. Same at main.rs:215 (`cmd_register`) and main.rs:395 (`cmd_remove`). The `RedactingMakeWriter` and `RedactingWriter` in `crates/shared/src/startup.rs` are built and tested but never installed — no tracing line is ever masked. Every secret (variables, `ACTIONS_RUNTIME_TOKEN`, registration token, broker body) lands in `_diag/runner.log` unredacted.
- **Fix:** Replace `startup::init(...)` with `startup::init_with_redactor(env!("CARGO_MANIFEST_DIR"), "runner", Box::new(SecretMasker::new()))` in all three CLI entry points. Pass the same `Arc<SecretMasker>` through to `GitHubListener` so per-job secrets from `job_runner.rs:106` also flow into it.
- **Refs:** `crates/toolu-runner/src/main.rs:298`, `crates/toolu-runner/src/main.rs:338`, `crates/shared/src/startup.rs:174`, `crates/execution/src/execution/secret_masker.rs:17`

### IMP-SE-002 — Step log lines uploaded to GH blob storage unredacted — secrets leak to the UI
- **Severity:** S | **Effort:** S
- **Rationale:** `execution_loop.rs:169-182` pushes raw log lines into the `StepLogStreamer` (gzipped and PUT to the signed SAS URL) and the live-log WebSocket without ever calling `masker.mask()`. With `IMP-SE-001` unfixed, every `::add-mask::` / `secrets.*` value echoed by a step ends up in the public job log.
- **Fix:** Wrap the `RunnerEvent::Log` branch in `execution_loop.rs:169` with `masker.mask(&line)` before pushing to `all_job_lines` / `uploaders[step_id]` / `live_log_tx` (apply once on the cloned line).
- **Refs:** `crates/listener/src/execution_loop.rs:169`, `crates/listener/src/log_uploader/streamer.rs:55`

### IMP-SE-003 — Path traversal in `LocalBackend::artifact_dir` allows arbitrary file write
- **Severity:** S | **Effort:** S
- **Rationale:** `execution/artifacts/backend.rs:70` joins `run_id` and `name` straight from the URL path / JSON body. A workflow POSTing `{"Name":"../../tmp/pwn"}` and PATCHing a chunk writes to `{root}/{run_id}/../../tmp/pwn/0.part`; `finalize()` then writes `artifact.bin` to that resolved path, escaping the artifacts root. Reachable from any workflow via the loopback `ACTIONS_RUNTIME_URL` service.
- **Fix:** Reject names/run_ids containing `..`, `/`, `\`, NUL, or absolute paths in `handle_create` / `handle_upload_or_finalize`; canonicalize the resolved path then assert it stays under `self.root` before any `create_dir_all`/`write`.
- **Refs:** `crates/execution/src/execution/artifacts/backend.rs:70`, `crates/execution/src/execution/artifacts/service/handlers.rs:44`

### IMP-SE-004 — Tar slip in `extract_tarball` — action tarballs can write outside `cache_dir`
- **Severity:** S | **Effort:** S
- **Rationale:** `execution/actions/downloader.rs:59` strips only the GitHub prefix component and then `dest.join(&stripped)`. If the tarball contains an entry like `evil-action/../../../../etc/cron.d/pwn`, the joined path escapes `dest`. `set_executable_if_needed` then chmods the file. A malicious or compromised action can overwrite any file the runner user can write.
- **Fix:** After stripping, walk the components of `stripped` and reject any `Component::ParentDir` or absolute prefix; or canonicalize `dest.join(stripped)` and assert it stays under `dest` (canonicalized) before writing.
- **Refs:** `crates/execution/src/execution/actions/downloader.rs:59`

### IMP-SE-005 — Raw response bodies and broker JWTs embedded in `RunnerError` → printed to stderr + `_diag/runner.log`
- **Severity:** S | **Effort:** S
- **Rationale:** `net/auth.rs:38`, `net/session.rs:30`, `net/messages.rs:60`, `net/run_service.rs:41,95,130`, `net/log_upload.rs:63,89,108` all do `format!("… failed: {body}")` where `body` is the upstream response text. OAuth/token-exchange endpoints routinely echo the request id; on some paths the body can include the JWT itself. `main.rs:122` then `eprintln!`s `{err}` and the error also lands in the (unwired, see `IMP-SE-001`) diag file.
- **Fix:** Stop including the raw body in user-visible errors — log it only at debug level (where masking applies when `IMP-SE-001` is fixed) and surface a generic message like `status {status} (see debug log)` to stderr. Sanitize any token-shaped substrings before logging.
- **Refs:** `crates/wire/src/net/auth.rs:38`, `crates/wire/src/net/session.rs:30`, `crates/toolu-runner/src/main.rs:122`

### IMP-SE-006 — Workflow command parser is dead code — `::add-mask::`, `::set-output::`, `::error::` never fire
- **Severity:** M | **Effort:** M
- **Rationale:** `execution/command_parser.rs:62` defines `parse_command` for `::add-mask::`, `::set-output::`, `::save-state::`, `::debug::`, `::error::`, `::warning::`, `::notice::`, `::group::`, `::stop-commands::` etc., but the function is never called anywhere (`rg parse_command` matches only the definition). Stdout from `script.rs:129` / `node_exec.rs:82` is forwarded raw to `RunnerEvent::Log` with no command dispatch.
- **Fix:** In `execution_loop.rs:169` (or a dedicated parser task), feed every incoming log line through `parse_command`; on `AddMask { value }` call `masker.add_secret(&value)`, on `SetOutput` route to `ctx.set_step_output`, on annotations emit `RunnerEvent::Annotation`.
- **Refs:** `crates/execution/src/execution/command_parser.rs:62`, `crates/execution/src/execution/handlers/script.rs:141`

### IMP-SE-007 — `RsaKeyParams` and `JitConfig` derive `Debug` — private key material leaks via `{:?}` formatting
- **Severity:** M | **Effort:** S
- **Rationale:** `protocol::types::RsaKeyParams` and `protocol::JitConfig` both `#[derive(Debug)]`. The struct contains the base64 big-endian RSA CRT components (`d`, `p`, `q`, `dp`, `dq`, `inverseQ`). Any future panic with `{jit_config:?}` or `{rsa_params:?}` (or library `unwrap` backtraces that print values) dumps the private key. No `Debug` redactor is registered.
- **Fix:** Replace `#[derive(Debug)]` on `RsaKeyParams` and `JitConfig` with a manual `impl Debug` that prints `redacted` / struct name only, or wrap fields in a `SecretString` newtype that redacts on `Debug`. Same treatment for `AccessToken` (`auth.rs:19`) and `OidcMode::Local.signing_key`.
- **Refs:** `crates/protocol/src/types.rs:66`, `crates/protocol/src/jit_config.rs:10`, `crates/protocol/src/auth.rs:19`

### IMP-SE-008 — Debug log emits broker message body + IV — enables offline AES-CBC decryption if AES key leaks
- **Severity:** M | **Effort:** S
- **Rationale:** `listener/job_lifecycle.rs:212` unconditionally does `tracing::debug!(body = %msg.body, iv = ?msg.iv, "raw broker message body")`. The body is the AES-CBC ciphertext of the encrypted job request. Combined with the per-session AES key (already leaked if any other secret path is breached) an attacker recovers the plaintext job request (which contains `SystemVssConnection` `AccessToken`). Triggered by any `RUST_LOG`/`TOOLU_RUNNER_LOG` directive that enables debug for the listener module.
- **Fix:** Drop the debug log, or gate it behind a `protocol-verbose` feature flag and never log `body`/`iv` (log only the `message_id` + `type`). When logged, redact through a registered redactor.
- **Refs:** `crates/listener/src/job_lifecycle.rs:212`

### IMP-SE-009 — `data_dir` and `.lock` created without `0o700` / `0o600` — readable by other local users
- **Severity:** M | **Effort:** S
- **Rationale:** `crates/shared/src/startup.rs:80,183` use `std::fs::create_dir_all(&diag)` with no mode — defaults to umask (typically 0755). `crates/config/src/lockfile.rs:112` also creates `.lock` without `.mode(0o600)`. On a shared host, any local user can read `_diag/runner.log` (full session secrets — see `IMP-SE-001`) or the `.lock` body. `config.toml`/`credentials.json` use `write_secret_file` which is correct, but the surrounding dirs are world-readable.
- **Fix:** After `create_dir_all`, explicitly `set_permissions(0o700)` on `data_dir` and `_diag`; on the `.lock` open in `lockfile.rs:112` add `.mode(0o600)` (mirroring `config.rs:45`). Same for every cache/artifacts/event subdir.
- **Refs:** `crates/shared/src/startup.rs:80`, `crates/config/src/lockfile.rs:112`, `crates/config/src/config.rs:167`

### IMP-SE-010 — `EnvFilter` accepts `RUST_LOG=trace` — user can flip the runner to dump secrets
- **Severity:** M | **Effort:** S
- **Rationale:** `crates/shared/src/startup.rs:222-227` builds the filter from `TOOLU_RUNNER_LOG` → `RUST_LOG` with no lower bound. A workflow / user setting `RUST_LOG=toolu_runner=trace` causes `IMP-SE-008`'s broker body+IV to be written to disk, plus every other debug-level value (masked or not — the masker is unwired anyway).
- **Fix:** Cap the filter at `info` by default and require an explicit opt-in (e.g. `TOOLU_RUNNER_ALLOW_VERBOSE=1`) to honor `debug`/`trace`. At minimum, refuse to honor trace-level directives from a config file or workflow input.
- **Refs:** `crates/shared/src/startup.rs:222`

---

## Performance / resource (top 10)

### IMP-PF-001 — Forwarder triple-clones every log line + retains entire job log in RAM
- **Severity:** S | **Effort:** M
- **Rationale:** `execution_loop.rs:169-181` clones each `RunnerEvent::Log` line 3× per dispatch (`all_job_lines.push`, streamer mpsc send, `LiveLogLine` struct), and the forwarder keeps every line of every step in `all_job_lines: Vec<String>` for the entire job. A 30-minute verbose job can pin hundreds of MB; the final gzip+PUT at line 226 happens after the job ends.
- **Fix:** Drop the `line.clone()` calls by routing via an enum that owns the data once (or pass `&str` + `Arc<str>`). Replace `all_job_lines` with a streaming writer — append each line to a temp file (or directly to a `flate2` encoder wrapped in a tokio file) and upload the finished blob at job end.
- **Refs:** `crates/listener/src/execution_loop.rs:135`, `crates/listener/src/execution_loop.rs:169`, `crates/listener/src/execution_loop.rs:226`

### IMP-PF-002 — Step/job log upload is single-shot BlockBlob — no streaming for large logs
- **Severity:** S | **Effort:** M
- **Rationale:** `upload_log_blob` (`net/results_service.rs:156`) does one PUT of the entire gzipped `Vec<u8>`. `UploadMode::AppendBlob` is defined in `reporting/log_upload.rs` but never reached on the real code path — `upload_compressed_step_logs` always calls `upload_log_blob`. A 100 MB step log becomes a 100 MB blocking PUT; on retry, `upload_blob_with_retry` clones the `Vec<u8>` again (`upload.rs:160`) which doubles peak memory.
- **Fix:** Switch to AppendBlob streaming: PUT create with `Content-Length: 0`, then PUT chunks of 4 MiB (the helper at `net/log_upload.rs:75` already implements this — just route through it). Drop `blob.clone()` in the retry path.
- **Refs:** `crates/wire/src/net/results_service.rs:156`, `crates/listener/src/log_uploader/upload.rs:154`, `crates/wire/src/net/log_upload.rs:75`

### IMP-PF-003 — `SecretMasker` scans every log line with O(patterns × line) `String::replace`
- **Severity:** M | **Effort:** M
- **Rationale:** `secret_masker.rs:54-65` sorts patterns longest-first then loops `result.replace(pattern, "***")` per pattern. Each `replace` allocates a new `String` and re-scans the whole line. The redactor runs on every tracing line. With N registered mask hints (each split into trimmed + per-line + json-escaped, so often 3-6× entries per secret) it's worst-case O(N×L) per log line, no Aho-Corasick.
- **Fix:** Build an `aho_corasick::AhoCorasick` (or `memchr::memmem` multi-pattern) once when secrets are added, then run a single pass over each line. Configure `MatchKind::LeftmostLongest` to keep the longest-match semantics.
- **Refs:** `crates/execution/src/execution/secret_masker.rs:54`, `crates/shared/src/startup.rs:29`

### IMP-PF-004 — Log rotation has no upper bound on retained files
- **Severity:** M | **Effort:** S
- **Rationale:** `startup.rs:85` uses `tracing_appender::rolling::daily(...)` which rotates daily but never deletes old files. A long-running runner accumulates `runner.log.YYYY-MM-DD` indefinitely. `README.md:246` even documents the unbounded archives.
- **Fix:** After rolling, glob `_diag/runner.log.*` in the diag dir, sort by mtime, and `remove_file` anything older than N days (e.g. 14). Hook this into a once-per-hour tokio task or piggyback on `tracing_appender`'s rolling policy with a custom guard.
- **Refs:** `crates/shared/src/startup.rs:85`, `docs/architecture.md:444`

### IMP-PF-005 — Poll backoff has no jitter — N runners synchronize into a thundering herd on 202
- **Severity:** M | **Effort:** S
- **Rationale:** `job_lifecycle.rs:116-145` resets `backoff` to 1s on every 202 `NoWork` but applies pure exponential backoff (1,2,4,8,16,32,60s) on `NetworkError` without jitter. A fleet of N runners polled by GH will all see the same 202 boundary and retry in lockstep — bursts the broker endpoint.
- **Fix:** Add full or decorrelated jitter: `sleep = rand(0..backoff)` on retry, or `sleep = backoff/2 + rand(0..backoff/2)`. Use `fastrand` (lightweight, no extra deps).
- **Refs:** `crates/listener/src/job_lifecycle.rs:116`, `crates/listener/src/job_lifecycle.rs:142`

### IMP-PF-006 — `is_pid_alive` does full `sysinfo` process-table refresh on every contended lock check
- **Severity:** M | **Effort:** S
- **Rationale:** `lockfile.rs:188-194` constructs a fresh `System`, calls `refresh_processes(ProcessesToUpdate::All, true)` — that's a syscall burst across `/proc` on Linux (or `proc_pidinfo` on macOS) enumerating EVERY process on the host, just to check one PID. Triggered every time a second `run` tries to acquire.
- **Fix:** Replace with a single `libc::kill(pid as i32, 0)` (or `nix::sys::signal::kill`); `ESRCH` → dead, `EPERM` → alive. One syscall, no enumeration.
- **Refs:** `crates/config/src/lockfile.rs:188`

### IMP-PF-007 — Expression evaluator does linear case-insensitive scan of every context on every property access
- **Severity:** M | **Effort:** M
- **Rationale:** `evaluator.rs:64-91` lowercases the requested name on every lookup, then linearly scans the `HashMap` with `k.to_ascii_lowercase() == lower` on every entry. With a typical job's `github` context having 30-60 keys, every `${{ github.foo }}` is O(N) string allocs + comparisons.
- **Fix:** At context build time, store a parallel `HashMap<String, usize>` mapping lowercase key → original key, plus precomputed lowercase values for direct string equality. Lookups become O(1) with zero allocation.
- **Refs:** `crates/expressions/src/evaluator.rs:64`, `crates/expressions/src/evaluator.rs:75`, `crates/execution/src/execution/context.rs:91`

### IMP-PF-008 — `report_step_to_results` fires one Twirp HTTP round-trip per step event
- **Severity:** M | **Effort:** M
- **Rationale:** `execution_loop.rs:194-203` calls `report_step_to_results` inside the event loop, awaiting one `update_workflow_steps` HTTP per event. A 100-step job is ≥200 sequential HTTPs (`StepStarted` + `StepCompleted` each). If GH is slow, the engine mpsc (capacity 1024) back-pressures the engine task.
- **Fix:** Move `report_step_to_results` into a separate spawned task fed by an `mpsc::channel`; let it buffer `StepStarted`/`StepCompleted` and batch them into a single `WorkflowStepsUpdateRequest` per `tokio::time::interval(500ms)` flush window. Cuts HTTPs ~100× and decouples engine from GH latency.
- **Refs:** `crates/listener/src/execution_loop.rs:194`, `crates/listener/src/helpers.rs:109`

### IMP-PF-009 — `RedactingWriter` builds `String` via `from_utf8_lossy` for every log line
- **Severity:** L | **Effort:** S
- **Rationale:** `startup.rs:289-293` + `322-329`: each completed line runs through `String::from_utf8_lossy(&line_bytes).into_owned()` (allocates + copies to a `String`) only for `redactor.redact(&line)` to also allocate. The line could stay as bytes through redaction by changing the redactor trait to take `&[u8]` and emit `Vec<u8>`.
- **Fix:** Change `SecretRedactor::redact` to take `&[u8]` and return `Vec<u8>`; rewrite replace operations on bytes directly using `memchr`/`aho_corasick`. Eliminates one allocation + one UTF-8 validation per line.
- **Refs:** `crates/shared/src/startup.rs:29`, `crates/shared/src/startup.rs:289`, `crates/shared/src/startup.rs:322`

### IMP-PF-010 — Lockfile: `fsync` per acquire + blocking `lock_exclusive` after stale recovery
- **Severity:** L | **Effort:** S
- **Rationale:** `lockfile.rs:172-178` writes the body then `f.sync_all()` on every successful acquire. On the stale-recovery path (`lockfile.rs:163`) it uses `file.lock_exclusive()` (blocking) — if another runner raced the `remove_file`, this blocks indefinitely instead of failing fast.
- **Fix:** Drop `sync_all` on the lock file — durability is not required for an advisory lock; only the kernel needs the inode kept open. After stale recovery, use `try_lock_exclusive()` (or `try_lock` with a short timeout).
- **Refs:** `crates/config/src/lockfile.rs:162`, `crates/config/src/lockfile.rs:177`

---

## Docs / ergonomics (top 10)

### IMP-DO-001 — Fix stale `--features e2e-live` in `test-coverage.md` (not README)
- **Severity:** M | **Effort:** S
- **Rationale:** The subagent audit originally flagged this as "README uses `e2e-live`, code uses `live`" — but the actual `crates/toolu-runner/Cargo.toml:89-92` defines `e2e-live` (not `live`). The README is correct. The real bug is in `docs/test-coverage.md:11` which says `cargo test -p toolu-runner --features live -- --ignored` — the feature name is wrong.
- **Fix:** Replace `docs/test-coverage.md:11` with `cargo test -p toolu-runner --features e2e-live -- --ignored`. Same fix on the `live` lane references at lines 18, 19, 20, 21, 22.
- **Refs:** `docs/test-coverage.md:11`, `docs/test-coverage.md:19`, `crates/toolu-runner/Cargo.toml:91`

### IMP-DO-002 — Test count claim in README + CHANGELOG is stale
- **Severity:** M | **Effort:** S
- **Rationale:** `README.md:290` and `CHANGELOG.md:169` both say "80 unit tests", but `docs/V1_STATUS.md:27` reports `167 unit tests + 8 live tests` at HEAD. This is the first thing a contributor checks; the gap undermines trust in the docs.
- **Fix:** Run `cargo test --workspace 2>&1 | tail -3` and paste the current number. Or replace both with a single sentence pointing at `docs/test-coverage.md` as the live source of truth.
- **Refs:** `README.md:290`, `CHANGELOG.md:169`, `docs/V1_STATUS.md:27`

### IMP-DO-003 — CLAUDE.md + CHANGELOG falsely list `service_auth` / `service_lifecycle` as cut
- **Severity:** S | **Effort:** S
- **Rationale:** `CLAUDE.md:58` and `:190` plus `CHANGELOG.md:200` claim these modules were cut and are "kept for ref but not wired". In reality they are wired: `crates/execution/src/execution/oidc/server.rs:17`, `crates/execution/src/execution/artifacts/service/handlers.rs:14`, `crates/cache/src/service/handlers.rs:14` all import them. A new contributor reading CLAUDE.md will think these files are dead code and delete them.
- **Fix:** Remove the bullet at `CLAUDE.md:58` and the line at `CLAUDE.md:190`; remove the entry from `CHANGELOG.md:200`. Add a one-liner under the execution engine section noting they back the OIDC/artifact/cache axum services.
- **Refs:** `CLAUDE.md:58`, `CLAUDE.md:190`, `CHANGELOG.md:200`, `crates/execution/src/execution/service_auth.rs:1`

### IMP-DO-004 — `tools/check.sh` line-limit claim in README is wrong (150 vs 700)
- **Severity:** M | **Effort:** S
- **Rationale:** `README.md:134` says `tools/check.sh` "rejects .rs files over 150 lines". The actual limit is 700 (`tools/check.sh:36`, with a comment "slightly relaxed to accommodate integration test harnesses"). New contributors editing a 300-line file will be confused why the gate passes despite the README.
- **Fix:** Change `README.md:134` to "rejects .rs files over 700 lines" and add a one-liner pointing at clippy's `too_many_lines` lint for the 150-line function-body cap (which IS clippy-enforced, per `tools/check.sh:30-31`).
- **Refs:** `README.md:134`, `tools/check.sh:36`

### IMP-DO-005 — `architecture.md` says `types/` re-exports `shared::RunnerConfig` but it shadows it
- **Severity:** M | **Effort:** S
- **Rationale:** `docs/architecture.md:45` said `src/types/ RunnerConfig (re-exported from shared)`. The actual file `toolu-runner/src/types/config.rs` (no longer exists — the `types/` dir was deleted in the crate split) declared its own struct with the same fields, and `toolu-runner/src/types/mod.rs` (also no longer exists) re-exported THAT, not the shared one. The two were kept structurally identical by convention, not by code. `CLAUDE.md:199` repeated the same claim.
- **Fix:** Resolved by deletion: `types/` was removed and call sites now use `shared::RunnerConfig` directly (the preferred, single-source-of-truth option originally proposed here).
- **Refs:** `docs/architecture.md:45`, `CLAUDE.md:199`, `toolu-runner/src/types/config.rs:1` (path no longer exists — see rationale)

### IMP-DO-006 — README quick-start has no no-sudo install path
- **Severity:** M | **Effort:** S
- **Rationale:** `install.sh` defaults to `/usr/local/bin` which is unwritable on a Linux box without sudo. `install.sh` supports `--install-dir`, but `README.md:16` only shows the `curl | sh` form. Linux users running the quick start on a fresh VM get "no write permission to `/usr/local/bin`" with no next-step pointer.
- **Fix:** Add a 4-line "no-sudo install" subsection under Quick start: `curl ... | bash -s -- --install-dir $HOME/.local/bin`, then `export PATH=$HOME/.local/bin:$PATH`. Reference the same `--install-dir` path the script already supports.
- **Refs:** `README.md:14`, `install.sh:39`

### IMP-DO-007 — `install.sh` has no "existing install" or "service already running" handling
- **Severity:** M | **Effort:** M
- **Rationale:** Re-running `install.sh` overwrites the binary without warning, doesn't stop a running launchd agent or systemd unit first (`install.sh:299` `launchctl load` is called without an `unload` for an already-loaded agent). README also has no "Upgrading" section. A user upgrading from v0.1.0 to v0.1.1 may end up with two agents fighting over `.lock`.
- **Fix:** In `install.sh:296` (darwin) and `install.sh:319` (linux), call `launchctl unload` / `systemctl stop toolu-runner.service` before install and emit a one-line "replaced `<old-version>` → `<new-version>`" notice using the `.runner_version` file the spec already tracks.
- **Refs:** `install.sh:296`, `install.sh:319`, `README.md:248`

### IMP-DO-008 — README troubleshooting section misses 5 common failure modes
- **Severity:** M | **Effort:** M
- **Rationale:** `README.md:265-283` covers config-not-found, registration-exists, lock-held, JIT probe, and `bash -n`. It misses every listener-side error path: JIT config base64/JSON parse failure, token-exchange 401 (revoked registration), session-create failure, poll-loop persistent network error, GHES `connectionData` fetch failure. A first-time user hitting any of these has no next-action pointer.
- **Fix:** Add bullets: `"JIT config base64 decode failed" — register wrote a stale/empty JIT blob, re-run register against a live GH repo.`; `"token exchange failed with status 401" — registration token expired or revoked; get a new one from Settings → Actions → Runners and re-run register.`; `"V1 discovery request failed" — check GHES hostname + that pipelines.<host> is reachable.`; `"another run is in flight; wrote ... marker" — but no PID/started_at shown (lock_held variant has them; main.rs:419 doesn't surface them).`
- **Refs:** `README.md:265`, `crates/toolu-runner/src/main.rs:419`, `crates/listener/src/handler.rs:130`

### IMP-DO-009 — README env-var table says "YAMLESS_* not recognized" but code calls it "legacy"
- **Severity:** L | **Effort:** S
- **Rationale:** `README.md:192` says "Not recognized. The runner prints a WARN..." but the actual warning text in `crates/shared/src/startup.rs:130` is `warning: ignoring legacy env var {key} — toolu-runner has no compatibility layer for the old prefix; use TOOLU_RUNNER_* instead`. The term `legacy` (now the canonical name — see `warn_about_legacy_env`, `scan_legacy_env`, `lefthook.yml::no-yamless-coupling` rename) doesn't appear in the README table.
- **Fix:** Replace the `README.md:192` row's Description with the actual warning text (so users searching docs for the literal message they see will find it), and rename the column "YAMLESS_* (any)" → "Legacy YAMLESS_* (any)".
- **Refs:** `README.md:192`, `crates/shared/src/startup.rs:130`, `docs/test-coverage.md:32`

### IMP-DO-010 — CHANGELOG doesn't record yamless → legacy rename or env-var warning behavior
- **Severity:** L | **Effort:** S
- **Rationale:** The codebase renamed `warn_about_yamless_env` → `warn_about_legacy_env` and the `no-yamless-coupling` check name. `test-coverage.md:32` notes the rename, but `CHANGELOG.md` has no entry for it. The `[Removed]` section (`CHANGELOG.md:208-210`) is still framed as yamless-only and doesn't mention the user-visible warning text.
- **Fix:** Add a one-line `[Changed]` bullet under 0.1.0: `Renamed warn_about_yamless_env → warn_about_legacy_env and the user-visible warning from "yamless" to "legacy"; behavior unchanged. Detection still triggers on the YAMLESS_ prefix.`
- **Refs:** `CHANGELOG.md:208`, `crates/shared/src/startup.rs:126`

---

## Appendix (not ranked)

Everything below the top-10-per-dimension cap. These are real
findings, just not the highest-ROI items to tackle first. Pick
from this list once the top-10s are burnt down.

### Code quality

- **IMP-CQ-A01** — `ResultsCtx` declared `pub` but only used inside the listener module. Fix: `pub(super)` for both the struct and its fields. Refs: `crates/listener/src/helpers.rs:29`
- **IMP-CQ-A02** — Token prefix (first 10 chars) logged in cleartext on results-service errors. Cross-ref **IMP-SE-015**. Refs: `crates/listener/src/helpers.rs:131`, `crates/listener/src/helpers.rs:137`
- **IMP-CQ-A03** — `let _ = std::fs::set_permissions` drops permission-set error silently. Fix: `tracing::warn!` before discarding. Refs: `crates/execution/src/execution/actions/downloader.rs:93`
- **IMP-CQ-A04** — `ExecutionContext::new_for_test` is the only constructor and is named for tests. Fix: rename to `new()` and add a `#[cfg(test)] with_test_defaults()`. Refs: `crates/execution/src/execution/context.rs:30`, `crates/execution/src/execution/job_runner.rs:97`
- **IMP-CQ-A05** — `placeholder pull_to_l1` creates an empty L1 entry after L2 hit, no data transfer. Fix: either implement the transfer or return `Err(Cache("L1->L2 replication not yet implemented"))` so callers see a clear error. Refs: `crates/cache/src/backend/layered.rs:82`, `crates/cache/src/backend/remote.rs:121`
- **IMP-CQ-A06** — GHES V1 URL resolvers are protocol-public but never called from production code (the V1 path is half-wired). Fix: add a thin orchestration layer in `listener` that calls `fetch_connection_data → resolve_service_url → fetch_timeline/post_timeline_record`, or demote the resolvers to `#[cfg(test)] pub`. Refs: `crates/protocol/src/v1/discovery.rs:9`, `crates/wire/src/net/v1.rs:55`
- **IMP-CQ-A07** — `let _ = events.send` pattern duplicated 25+ times to swallow mpsc send errors. Fix: add `fn emit_event(events: &mpsc::Sender<RunnerEvent>, event: RunnerEvent)` helper. Refs: `crates/execution/src/execution/steps_runner.rs:75`, `crates/execution/src/execution/job_runner.rs:65`, `crates/listener/src/execution_loop.rs:172`

### Test gaps

- **IMP-TG-011** — transport header-builders (`block_blob_headers`, `append_block_headers`, `create_append_blob_headers`) not covered by `wiremock`. Fix: add a `#[tokio::test]` asserting the request contains `x-ms-blob-type: BlockBlob` for compressed payloads. Refs: `crates/wire/src/net/log_upload.rs:12`
- **IMP-TG-012** — `reporting::feature_detection` has no test. Fix: build an `AgentJobRequestMessage` with and without `run_service_url` and assert V2/V1 respectively. Refs: `crates/wire/src/reporting/feature_detection.rs:37`
- **IMP-TG-013** — V1 service URL resolvers (`log_files_url`, `log_lines_url`, `job_finish_url`, `agent_delete_url`) not covered. Fix: extend `ghes_v1_test` with a test that feeds a `ConnectionData` containing all five V1 GUIDs. Refs: `crates/protocol/src/v1/discovery.rs:25`
- **IMP-TG-014** — `PluginRegistry` behavior beyond construction untested. Fix: add unit tests for dedup (re-registering a name replaces) and `iter()` insertion order. Refs: `crates/execution/src/plugin/registry.rs:21`
- **IMP-TG-015** — `LocalDiskBackend` (cache LRU) has no test. Fix: add a `#[tokio::test]` that reserves a cache id, uploads 1024 bytes, finalizes, downloads, and asserts roundtrip; plus a second entry that triggers eviction when total > 1024. Refs: `crates/cache/src/backend/local_disk.rs:33`
- **IMP-TG-016** — `LocalBackend` (artifacts) has no test. Fix: add a default-lane test that uploads 3 chunks, calls `finalize`, then `download`, and asserts the concatenated content equals the concatenated input bytes in order. Refs: `crates/execution/src/execution/artifacts/backend.rs:57`
- **IMP-TG-017** — cache key + trust classifier untested. Fix: add a default-lane test for `key_matches` covering exact match, trailing-colon prefix match, and non-match. Refs: `crates/cache/src/key.rs:35`
- **IMP-TG-018** — `PathTranslator` untested. Fix: add a default-lane test calling `to_container(host_workspace.join("foo.txt"))` and asserting `/github/workspace/foo.txt`, plus the reverse. Refs: `crates/execution/src/docker/path_translator.rs:27`
- **IMP-TG-019** — `hash_files` expression function untested. Fix: add a default-lane test against a tempdir containing two known files, asserting `hash_files(tmp, &["*.txt"])` produces a 64-hex-char SHA-256 string. Refs: `crates/expressions/src/functions/hash.rs:13`
- **IMP-TG-020** — lex/parser unicode and deep-nesting edge cases untested. Fix: add a default-lane test asserting: hyphenated property `github.event.head-commit` parses; 6-level deep chain evaluates; unterminated string returns `Expression` error. Refs: `crates/expressions/src/lexer.rs:125`
- **IMP-TG-021** — `is_runner_deregistered` never exercised. Fix: inline test covering both matched (`"404"` + `"RunnerNotFound"`) and unmatched inputs. Refs: `crates/listener/src/handler.rs:141`

### Security

- **IMP-SE-011** — `SecretMasker.add_secret` does not register base64 / double-JSON / URL-encoded variants. Fix: auto-register `base64(value)`, URL-encoding, single-quote shell escaping, double-quote shell escaping, second pass of JSON escaping. Refs: `crates/execution/src/execution/secret_masker.rs:67`
- **IMP-SE-012** — OIDC `signing_key` and RSA DER bytes held as plain `Vec<u8>` — no `zeroize` on drop. Fix: wrap in `zeroize::Zeroizing<Vec<u8>>` (or sealed `SecretBytes` newtype). Refs: `crates/execution/src/execution/oidc/claims.rs:14`, `crates/protocol/src/auth.rs:50`
- **IMP-SE-013** — `validate_bearer` uses `!=` (non-constant-time) — local timing oracle. Fix: `subtle::ConstantTimeEq::ct_eq(...).into()`. Refs: `crates/execution/src/execution/service_auth.rs:12`
- **IMP-SE-014** — `build_jwt` reads `SystemTime::now()` with no monotonic / skew sanity check. Fix: validate `now` against a recent persisted baseline; refuse to mint if skew > N seconds. Refs: `crates/protocol/src/auth.rs:101`, `crates/execution/src/execution/oidc/claims.rs:113`
- **IMP-SE-015** — 10-char prefix of `x-actions-results-token` logged on every results-service error. Cross-ref **IMP-CQ-A02** and **IMP-DO-014**. Fix: drop the prefix entirely; if needed, hash via `blake3::hash(token).to_hex()[..8]` for a stable fingerprint. Refs: `crates/listener/src/helpers.rs:131`
- **IMP-SE-016** — Action downloader calls `actions.githubusercontent.com` without TLS-pinning or checksum verification. Fix: hash the bytes (sha2 or blake3) and compare against the digest published in the action's GitHub release metadata. Refs: `crates/execution/src/execution/actions/downloader.rs:115`
- **IMP-SE-017** — `GITHUB_ENV` / `GITHUB_PATH` file-command parsing allows arbitrary env-var names (`LD_PRELOAD`, `PYTHONPATH`, `BASH_ENV`, `IFS`, etc.). Fix: maintain an allow/deny list mirroring upstream `actions/runner` rules (block `LD_*`, `DYLD_*`, `BASH_ENV`, `BASH_FUNC_*`, `IFS`, `PS*`, `PYTHONPATH`, `RUBYLIB`; `NODE_OPTIONS` already blocked). Refs: `crates/execution/src/execution/file_commands.rs:145`
- **IMP-SE-018** — `TRACEPARENT`/`TRACESTATE` injected into env but no W3C-trace-context validation. Fix: validate incoming `traceparent` (128-bit hex trace-id, 64-bit hex span-id, 2-hex flags, single-byte trace-flags) before honoring; reject malformed. Refs: `crates/execution/src/execution/job_runner.rs:48`
- **IMP-SE-019** — Step env inherits the full runner process env via `envs(std::env::vars())`. Fix: whitelist the env keys passed through; at minimum drop `TOOLU_RUNNER_*`, `RUST_LOG`, and any `_TOKEN`/`_KEY`/`_SECRET`-suffixed var unless explicitly included. Refs: `crates/execution/src/execution/handlers/script.rs:50`, `crates/execution/src/execution/handlers/node_exec.rs:38`

### Performance

- **IMP-PF-A01** — `build_step_env` clones full env `HashMap` per step. Fix: hold merged env in an `Arc<HashMap>` and clone the `Arc` per step. Refs: `crates/execution/src/execution/context.rs:200`, `crates/execution/src/execution/steps_runner.rs:134`
- **IMP-PF-A02** — `gzip_lines` uses `writeln!(encoder, "{line}")` per line. Fix: `encoder.write_all(line.as_bytes())?; encoder.write_all(b"\n")?;`. Refs: `crates/listener/src/log_uploader/upload.rs:80`
- **IMP-PF-A03** — Per-step log uploader mpsc has capacity 4096 — blocks the forwarder on bursts. Fix: raise capacity or drop excess lines into a small overflow buffer. Refs: `crates/listener/src/log_uploader/streamer.rs:13`
- **IMP-PF-A04** — `event.json` write is synchronous `serde_json::to_string_pretty` on the engine task. Fix: `tokio::task::spawn_blocking` + `serde_json::to_writer_pretty` into a `tokio::fs::File`. Refs: `crates/execution/src/execution/job_runner.rs:142`
- **IMP-PF-A05** — `PipelineContextData → ExprValue` clones every nested value recursively. Fix: use `Rc<ExprValue>` / `Arc<ExprValue>` for the event subtree. Refs: `crates/expressions/src/context_data.rs:13`
- **IMP-PF-A06** — `format!("{err}")` in `is_runner_deregistered` allocates just to substring-match. Fix: match on the error variant (add a `RunnerNotFound` variant) or expose HTTP status as a typed field. Refs: `crates/listener/src/handler.rs:141`
- **IMP-PF-A07** — `load_dotenv` runs on every CLI subcommand including `status`. Fix: skip `.env` for read-only subcommands, or cache in a `OnceLock` per process. Refs: `crates/shared/src/startup.rs:74`, `crates/shared/src/startup.rs:230`
- **IMP-PF-A08** — `poll_message` constructs a fresh URL string every poll. Fix: build the query suffix once (e.g. `static QUERY_SUFFIX: OnceLock<String>`) and concatenate with the broker URL on each call. Refs: `crates/wire/src/net/messages.rs:35`
- **IMP-PF-A09** — listener tx channel capacity 256 + engine tx capacity 1024 can serialize the pipeline. Fix: bump both (engine to 4096, listener to 1024) and/or add the batching dispatch proposed in `IMP-PF-008`. Refs: `crates/listener/src/handler.rs:76`, `crates/execution/src/lib.rs:61`

### Docs / ergonomics

- **IMP-DO-011** — Missing step-by-step first-run guide. Fix: add `docs/GETTING_STARTED.md` (~80 lines): a 6-step checklist with the exact GH web-UI navigation, expected stdout of each step, and a "next" link to README's troubleshooting. Refs: `README.md:12`
- **IMP-DO-012** — `register --token` help doesn't warn the token is single-use. Fix: update `main.rs:60` doc line to: `/// Short-lived, single-use registration token from Settings → Actions → Runners. Burns on first successful register.` Refs: `crates/toolu-runner/src/main.rs:60`
- **IMP-DO-013** — "registration already exists" error doesn't point at the GH-side runner. Fix: extend the error at `main.rs:235` to include `Visit https://github.com/<owner>/<repo>/settings/actions/runners to remove the previous registration, or pass --replace to overwrite locally (the GH-side runner is not deleted until step 10's unregister call lands).` Refs: `crates/toolu-runner/src/main.rs:232`
- **IMP-DO-014** — `results_service` step-update failure logs OAuth token prefix to stderr. Cross-ref **IMP-SE-015**. Refs: `crates/listener/src/helpers.rs:131`
- **IMP-DO-015** — `cmd_remove` "another run is in flight" message drops holder's PID/started_at. Fix: at `main.rs:417-422`, parse the lock body and append `Holder PID: <pid>, started_at: <started_at>` to the error. Refs: `crates/toolu-runner/src/main.rs:417`, `README.md:270`
- **IMP-DO-016** — `cmd_run` acquires `.lock` before checking empty `jit_config`. Fix: move the `if jit_config_b64.is_empty()` check (main.rs:345) above the `lockfile::acquire` call (main.rs:330). Refs: `crates/toolu-runner/src/main.rs:330`, `crates/toolu-runner/src/main.rs:345`
- **IMP-DO-017** — `PluginRegistry::new` has no `///` doc line. Fix: add `/// Construct an empty registry. Plugins can be added with [`Self::register`].` Refs: `crates/execution/src/plugin/registry.rs:14`
- **IMP-DO-018** — `register --once` and `--work` flags are absent from README. Fix: add a 3-row "CLI flags reference" subsection to README.md under the Quick start, listing `--once` (run), `--work` (register), `--runner-group` (register), `--force` (remove), `--replace` (register). Refs: `README.md:12`, `crates/toolu-runner/src/main.rs:88`
- **IMP-DO-019** — `scripts/test/plist_test.sh` misses `--no-config` validation. Fix: grep the `--config` value out of the plist and assert the file exists (after install). Refs: `scripts/test/plist_test.sh:1`, `README.md:152`
- **IMP-DO-020** — `install.sh` download-failed diagnostic doesn't mention `TOOLU_RUNNER_REPO`. Fix: append one line: `If you're installing from a fork, set TOOLU_RUNNER_REPO=<owner/repo> before running.` Refs: `install.sh:223`
- **IMP-DO-021** — Step handler dispatch description in docs is inconsistent (CLAUDE vs `handlers/mod.rs`). Fix: align `handlers/mod.rs:13` with CLAUDE.md:49 (plugin is a real entry point via the `PluginRegistry`). Refs: `CLAUDE.md:49`, `crates/execution/src/execution/handlers/mod.rs:11`
- **IMP-DO-022** — `docs/architecture.md:154` session delete semantics claim is vague. Fix: replace with: `Status non-2xx on delete returns RunnerError::Protocol; transport errors propagate. The caller in listener/helpers.rs:105 logs and continues (broker may have expired the session already).` Refs: `docs/architecture.md:154`, `crates/wire/src/net/session.rs:44`

---

## Next step

This is the brainstorm output. The next phase is `spec` (or
straight to `plan` for a smaller burn-down). Recommended
approach: pick the Cross-cutting top 5 as a single sprint, plan
each as a self-contained PR, and use the per-dimension top 10s
as the next sprint's backlog.
