# Test Coverage Map

> **Epic-wide acceptance status lives in
> [compatibility-matrix.md](compatibility-matrix.md)** (generated from
> `compatibility-matrix.json`, validated in CI). It records one row per child
> AC/scenario with all seven lanes, and [compatibility-evidence.md](compatibility-evidence.md)
> explains lanes, capture/sanitization and the official-runner reference lane.
> The table directly below is the original v1 design-spec map; a `live` entry
> there means a test exists, not that it ran.

Maps each of the 23 acceptance criteria from the design spec to the
test(s) that cover it, with a "lane" indicating whether the test runs in
the default `cargo test --workspace` lane or only with the `live`
feature enabled.

Lane legend:

- **default** — runs in `cargo test --workspace` (hermetic, no network).
- **live** — runs in `cargo test -p toolu-runner --features live -- --ignored`
  (requires a real test repo + PAT).
- **out-of-scope** — covered by a non-test artifact (install script,
  CI gate, design spec).

| AC  | Description                                      | Test(s)                                                                                                                                                                                                                                                                                                                                                       | Lane         |
| --- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ |
| 1a  | Local status output (no network)                 | `crates/toolu-runner/tests/cli_test.rs::help_lists_all_subcommands` (clap surface), `crates/toolu-runner/tests/failure_modes_test.rs::legacy_env_warning_emitted_to_stderr_when_var_set` (exercises the `status` subcommand end-to-end through the binary).                                                                                                            | default      |
| 1b  | Runner online within 30s of `register`           | `crates/toolu-runner/tests/live_e2e.rs::register_creates_config_and_credentials`, `crates/toolu-runner/tests/live_e2e.rs::register_replace_overwrites_existing` (gated; verifies config + credentials on disk after a real `register` call against the GH API).                                                                                       | live         |
| 2   | No-op job (`run: echo hello`) completes          | `crates/toolu-runner/tests/live_e2e.rs::noop_job_completes` (gated; dispatches the noop workflow, runs the runner, asserts `conclusion: success` on the GH run).                                                                                                                                                                                                  | live         |
| 3   | Multi-step job (run, uses, docker://)            | `crates/toolu-runner/tests/live_e2e.rs::multi_step_job` (gated; pushes `tests/fixtures/multistep-workflow.yml`: checkout + setup-node + a pinned `docker://alpine` container action whose Alpine-only output and marker a later step asserts). Real container execution is evidenced by `crates/execution/tests/compat_probe_replay_test.rs` (CI Docker step) and the #103 reference run; the live test itself is unverified while no live token is configured. | live         |
| 4   | Action resolution (`actions/checkout@v4`)        | `crates/toolu-runner/tests/live_e2e.rs::action_resolution` (gated), `crates/toolu-runner/tests/actions_resolver_test.rs` (planned; see `crates/execution/src/execution/actions/resolver.rs::parse_action_ref` and `resolve_action_refs` for the parser-level coverage).                                                                                                  | live         |
| 5   | Expression evaluation (`${{ }}` full syntax)     | `crates/toolu-runner/tests/live_e2e.rs::expression_evaluation` (gated; `tests/fixtures/expression-workflow.yml` asserts the exact typed inputs, and a second dispatch with `who: ''` must conclude `failure`), `crates/toolu-runner/tests/expression_eval_test.rs` — 28 tests covering literals, context/property/index access, function calls (`contains`, `startsWith`, `endsWith`, `format`, `join`, `toJson`, `fromJson`, `success`, `failure`, `always`, `cancelled`), binary ops, unary `!`, wildcards, and parse errors.                                                       | default+live |
| 6   | Secret masking in logs                           | `crates/toolu-runner/tests/secret_masker_real_test.rs` — 10 tests driving the real `SecretMasker` against recorded log fixtures (`tests/fixtures/secret-masking-input.txt` → `secret-masking-expected.txt`), JSON-escaped variants, multi-line secrets, longest-first replacement, and the full `RedactingWriter` pipeline.                                                | default      |
| 7   | OIDC token issuance                              | `crates/toolu-runner/tests/oidc_token_test.rs` — 6 tests spinning up the real `OidcServer` axum endpoint on `127.0.0.1:0`, posting to `/_apis/pipeline/oidc/requestToken`, decoding the returned JWT, verifying the default and overridden audiences, the 10-min expiry, and bearer-token auth.                                                                       | default      |
| 8   | Artifact upload + download                       | `crates/execution/src/execution/artifacts/` (types only; v1 artifact service lives in `execution::execution::artifacts`). Live flow gated on artifact end-to-end tests landing in step 10.                                                                                                                                                                       | out-of-scope |
| 9   | Cache save + restore                             | `crates/cache/src/` (types only; v1 cache service lives in the `cache` crate). Live flow gated on cache end-to-end tests landing in step 10.                                                                                                                                                                                      | out-of-scope |
| 10  | Reusable workflows                               | Server-side: GitHub resolves `uses:` workflow calls and sends the expanded job; the local parser/resolver was removed in #97. Context evidence: `expression-context-call.yml` (see below). | n/a (server-side) |
| 11  | Composite actions                                | `crates/toolu-runner/tests/composite_action_test.rs` — 12 tests parsing a real-shape composite `action.yml` (with `inputs`, `outputs`, `runs.using: composite`, multi-step `steps[]`), preparing composites (scope + depth tracking, depth limit), evaluating `${{ steps.X.outputs.Y }}` output expressions, and unsupported `runs.using` rejection.   | default      |
| 12  | Reconnect after transient network failure        | `crates/toolu-runner/tests/net_test.rs` — `exchange_token_returns_protocol_error_on_http_failure` (401) and `poll_message_returns_none_on_202` (long-poll no-work). Listener-side exponential backoff in `crates/listener/src/job_lifecycle.rs` (covered by the listener smoke test). Live mid-job outage scenarios in the design spec are tracked in `docs/known-bugs.md`. | default      |
| 13  | Concurrent single-job guarantee (file lock)      | `crates/toolu-runner/tests/failure_modes_test.rs::lock_acquire_writes_body_and_releases_on_drop`, `lock_conflict_returns_held_pid`, `lock_replaces_stale_lock_when_holder_pid_dead`. Live two-process race in `crates/toolu-runner/tests/live_e2e.rs::concurrent_single_job` (gated).                                                                                  | default      |
| 14  | Cancel by GH (UI cancel → 30s)                   | `crates/toolu-runner/tests/live_e2e.rs::cancel_by_github` (gated; dispatches a `sleep 300` job, calls `POST /actions/runs/{id}/cancel`, asserts `conclusion: cancelled` within 60s).                                                                                                                                                                              | live         |
| 15  | GHES V1 protocol                                 | `crates/toolu-runner/tests/ghes_v1_test.rs` — 6 tests driving `wire::net::v1::{fetch_connection_data, post_timeline_record, fetch_timeline}` against a `wiremock` server returning the real V1 service-discovery shape, plus the `protocol::v1::resolve_service_url` URL resolver exercised end-to-end.                                                          | default      |
| 16  | Service install (launchd + systemd)              | `install.sh` (single shell script that emits a launchd plist on macOS, a systemd unit on Linux). Manual smoke only — no automated test for the service files.                                                                                                                                                                                                   | out-of-scope |
| 17  | Install script                                   | `tests/install_sh_test.sh` (CI release-script step, not cargo): real loopback release fixtures, SHA256SUMS verification, PATH env files checked by bash/zsh/fish login shells, `--uninstall`, setup handoff; `tests/install_sh_bash32_test.sh` runs it under macOS `/bin/bash` 3.2 on ci-macos (#200).                                                         | out-of-scope |
| 19  | Lint + typecheck gate                            | `lefthook.yml::pre-commit::rust-clippy` runs `cargo clippy --workspace --all-targets -- -D warnings`; `tools/check.sh all` runs `cargo fmt --all -- --check` + clippy + file-size + no-allow + no-unwrap. The `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy` invocations in the build pipeline are the live gate.            | out-of-scope |
| 20  | Live bug tracking                                | `docs/known-bugs.md` is the single source of truth; updated when the listener smoke test or live tests surface a behavior gap.                                                                                                                                                                                                                                | out-of-scope |
| 21  | Protocol crate is sync + I/O-free                | `crates/protocol/Cargo.toml` dep list is restricted to `serde*`, `base64`, `jsonwebtoken`, `num-bigint-dig`, `pkcs1`, `sha1`, `sha2`, `aes`, `cbc`, `uuid` (no `reqwest`, `tokio`, `opendal`, `bollard`, `axum`). `crates/protocol/tests/integration.rs` is `#[test]` only — no `#[tokio::test]`, no `wiremock`.                                                                  | default      |
| 23  | Real-data test suite                             | This file. The 5 new test files (`expression_eval_test.rs`, `secret_masker_real_test.rs`, `oidc_token_test.rs`, `composite_action_test.rs`, `ghes_v1_test.rs`) plus the recorded fixtures under `tests/fixtures/` form the suite. Live tests under `tests/live_e2e.rs (harness: tests/helpers/)` complete the picture with `--features live`.                    | default+live |

## Recorded fixtures

`crates/toolu-runner/tests/fixtures/`:

- `jit_config_github_com.json` — base64-encoded JIT envelope (3-blob: `.runner` / `.credentials` / `.credentials_rsaparams`) for a github.com host. Includes `expected_server_url_v2`, `expected_client_id`, `expected_authorization_url` for the live tests to assert against.
- `jit_config_ghes.json` — same shape but with `host_kind: "ghes"` (no `run_service_url`, `ServerUrl`/`ServerUrlV2` rooted at `ghes.example.com`). Exercises the V1 path.
- `broker_message_job_request.json` — real-shape `BrokerMessage` envelope (`messageId` / `messageType: "RunnerJobRequest"` / `body` / `iv`) with a synthetic job-request body base64-encoded. `body` decrypts to a `RunnerJobRequestBody`-shaped JSON.
- `broker_message_migration.json` — `BrokerMessage` with `messageType: "BrokerMigration"`. The body is base64-encoded JSON containing `brokerBaseUrl` (the URL the listener would switch to on a migration).
- `secret-masking-input.txt` — six real-shape log lines mixing bare tokens, bearer `ghp_` tokens, JSON-stringified tokens, and a JSON-escaped variant. Drives the `SecretMasker` round-trip.
- `secret-masking-expected.txt` — the same six lines with secrets replaced by `***`. Used as the test's expected output.

`crates/toolu-runner/tests/fixtures/`:

- `noop-workflow.yml` — `run: echo hello` workflow. Pushed by `crates/toolu-runner/tests/live_e2e.rs::noop_job_completes` to the test repo before dispatch.

Every workflow `live_e2e.rs` pushes is a committed fixture (`noop-`, `multistep-`, `expression-`, `journal-`, `action-resolution-`, `cancel-workflow.yml`); `scripts/test/live_lane_test.sh` (CI) keeps inline YAML out and pins the Docker/input assertions. Live tests fail with `UNVERIFIED: …` when `TOOLU_RUNNER_LIVE_TOKEN` / `TOOLU_RUNNER_LIVE_REPO` are absent; they never skip as a pass.

## Summary

- 23 ACs total.
- 15 ACs covered by default-lane tests (no network).
- 5 ACs covered by live-only tests (require a real test repo).
- 3 ACs are out-of-scope for the test suite (install script, service files, lint gate — enforced by `tools/check.sh` and `lefthook.yml`).

Total test count after step 11: see `cargo test --workspace` summary
below. Live tests compile under `--features live` but only run with
`--ignored` and the required env vars (`TOOLU_RUNNER_LIVE_TOKEN`,
`TOOLU_RUNNER_LIVE_REPO`).

## Issue #86 — explicit multiline workflow masks

`::add-mask::` registers the decoded whole value exactly and every nonempty
trimmed CR/LF-separated line, including one-character values. Empty and
whitespace-only commands register nothing. The general four-byte policy for
message secrets remains unchanged. Explicit short masks can redact ordinary
text; callers intentionally request that behavior.

The real-tool regression is
`crates/listener/src/tests/multiline_mask.rs::multiline_mask_real_job_reaches_every_local_sink`,
run with `cargo nextest run -p listener -E 'test(multiline_mask)'`. It replays
the committed sanitized `crates/toolu-runner/tests/fixtures/job_message.json` envelope through
`run_job`, preserving the first two step UUID/contextName identities and
replacing their inputs with the disposable shell probe
`crates/listener/tests/multiline_mask.sh` and local composite actions. This is
local production replay, not a newly captured live GitHub job. Probe values
are deliberately non-credential text. The behavior oracle is
[actions/runner cab9d1c, ActionCommandManager](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/ActionCommandManager.cs#L444).

| Scenario | Input and exact expected observation | Evidence scope |
| --- | --- | --- |
| 86-S1 | Encoded LF/CRLF/CR, blank lines, whitespace, repeated/overlapping values, 1–3 byte values and Unicode; whole value and each trimmed line become `***`; a plain space stays unchanged. `%250A` is decoded only once. | Real shell → dispatcher → shared masker. |
| 86-S2 | Seven stdout and seven stderr records become `stdout=[***]` / `stderr=[***]`, with `control remains visible` unchanged. | Actual event forwarder produces identical combined-job, step-upload and live-log buffers; real journal writer persists matching JSONL lines; production `RedactingWriter` with `MaskerRedactor` yields identical diagnostic lines. No HTTP service or upload acknowledgment is mocked. |
| 86-S3 | Nested local composite invokes background shell writers after registration; repeated/overlapping values merge without residual text. | Sorted probe records match exactly; stream identities, counts and successful job completion are asserted. Concurrent registration is serialized by the existing shared mutex. |
| 86-S4 | A later top-level notice containing three newly registered short lines becomes `annotation=[***|***|***]`. | Annotation producer covered locally. Summary, job output and deployment-URL integration with #70/#82/#83/#88 remains **unverified**. |

Verification status: the pre-fix test reproduced cleartext
`annotation=[Q|RS|TUV]` on macOS. The post-fix production-replay test passes
on macOS with nextest, and the full `./tools/check.sh all` gate passes.
Ignored live tests remain unverified.
Linux execution, GitHub.com/GHES UI/backend uploads and comparison with the
pinned official runner are **unverified**. Registration is backend-independent;
these local buffers do not prove remote UI behavior. Diagnostics here exercise
the actual writer/redactor directly, not global subscriber initialization.
Nested composite shell workflow-command routing is a separate observed gap;
the nested probe tests ordinary stdout/stderr, and the notice runs top-level.

## Incoming contexts acceptance (#68)

The issue-specific `expression-live.yml` workflow uses label `toolu-68`. A real
GitHub-hosted producer emits `artifact-68`; two self-hosted consumer children
assert their own matrix values, exact needs output/result, dispatch inputs and
strategy fields. `expression-context-call.yml` asserts reusable-workflow inputs
`called`, numeric `0`, and boolean `true`. The committed parent/child composite
actions assert workflow/action input separation across nested and sibling calls,
then the workflow verifies the resulting files and its restored input scope.

Dispatch with `gh workflow run expression-live.yml --repo Falconiere/toolu-ghrunner
--ref <branch> -f who=world -f count=3 -f enabled=false` (one command). GitHub
actually delivered dispatch `count` as string `"3"`, despite its numeric input
declaration; the reference runner preserved it. A REST dispatch with JSON numeric
`3` was rejected (422). The assertions preserve that observation; matrix numbers
and reusable-workflow numeric `0` exercise numeric values without coercion.

| Requirement / scenario | Real input and exact result | Production check |
| --- | --- | --- |
| Incoming contexts; 68-S1 / S4 | Matrix alpha/7/true and beta/0/false; needs artifact-68/success; strategy index0/1, total2, max1, fail-fastfalse; dispatch world/string3/false and call called/number0/true | `acquired_contexts_retain_values_and_types`, `acquired_matrix_jobs_execute_assertions_and_restore_composite_scope`, `acquired_workflow_call_executes_typed_input_assertions` |
| Unknown contexts; 68-S2 | Captured values moved to future root names retain zero/false/string/nested empty array. Removing roots, replacing with null, or emptying a dictionary preserves null versus object. Mixed-case runtime root collisions do not replace github/vars/env/secrets/steps/runner/job or import private runner env | `new_roots_keep_nested_types_and_cannot_shadow_runtime_roots`, `absent_null_and_empty_roots_remain_distinct` |
| Composite input scope; 68-S3 | Parent world, nested inner, sibling sibling, then parent workflow world; exact output files prove each shell ran | Matrix production replay and identical committed actions on both live runners |
| Captured-data coverage | Three sanitized raw Run Service acquisitions retain job UUIDs, context names, nested values, and template token types | `crates/execution/tests/incoming_contexts_test.rs`; `python3 scripts/test/context_capture_check.py` |
| Repository gate and docs | Full workspace formatting/lint/guardrail/test command; architecture documents incoming versus runtime ownership | `./tools/check.sh all`; `docs/architecture.md` |

All Rust rows run under `./tools/check.sh all`. Replay enters `Runner::execute_job`
and the real `run_job`/step loop, shells and composite actions. It omits checkout
and prepopulates the exact committed local action files; all context-bearing
assertion steps remain unchanged. The S2 boundary cases are explicitly labelled
transformations of captured messages, not additional GitHub captures. They test
assembly/evaluation; the unmodified replay and live jobs prove engine wiring.
`incoming_contexts_node_test.rs` additionally routes the captured expression-bearing
action step to the committed real Node probe (`incoming_contexts_node_action.*`).
It checks actual `INPUT_*` values/output JSON, a selected manifest default, a
literal expression-looking result that must not be evaluated twice, and malformed
expression failure before the action runs. This is a labelled capture-derived
Node variant, not an additional live GitHub capture. It requires a real `node`
on PATH (no passing skip); the cache is seeded with that executable to avoid a
runtime download. Runtime version selection is not tested by this probe; the
local run used Node 26.9.0.

Display-name and timeout token evaluation remain owned by #99, and complete
composite expression/cleanup semantics by #102. The committed workflow establishes
server acceptance for the tested if/env/with/script/working-directory sites.

The raw fixtures were captured from [run 36038637634](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36038637634)
at workflow/action revision `e607a7aae26e113aa3d8ecf1c26c62e0d74192eb`, using a
private temporary acquisition probe in the unmodified toolu runner. That run
failed as expected and is fixture provenance, not passing acceptance. The probe
was removed before implementation. Sanitization replaces secret variables,
endpoint authorization, mask values and token-bearing URLs, preserving structure
and types. Every secret variable, endpoint authorization parameter, and mask
value uses a distinct synthetic UUID, retaining the captured `isSecret` flags.
Each UUID is UUIDv5 with the standard URL namespace and the name
`toolu-ghrunner/68/<fixture filename>/<field path>`, where the field path joins
JSON keys and zero-based array indexes with `/`. The capture checker pins those
exact placeholders and rejects the former generic marker. Its colocated tests
use these captures and verify rejection of an incorrect token or changed secret
flag. Fixtures and workflow files have
SHA-256 records in `crates/execution/tests/incoming_contexts_evidence.json`.

The official runner was built from the exact epic pin
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5` (not just a similarly named release).
[Reference run 36038462880](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36038462880)
passed every assertion on that workflow/action revision. The evidence JSON records
its Listener/Worker assembly hashes. Both lanes use the same macOS 26.6.2 ARM64
host, Bash 5.3.20 and jq 1.7.1; reference checkout uses bundled Node 20.20.2 and
toolu checkout uses Node 20.18.3. Context and composite assertions use the real shell.

[Toolu run 36042145619](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36042145619)
passed all four jobs with the same workflow/action revision after rebasing onto
main. The evidence JSON records the exact rebased source commit and binary hash. The live checker
verified both runs, required assertion steps, runner identities and content hashes.
The full repository gate passed with all seven captured-context/replay and real-Node
boundary tests. The live checker
`python3 scripts/test/context_capture_check.py --live` requires authenticated `gh`,
checks exact revisions, runner identities, four successful jobs and their required
assertion steps, and compares workflow/action contents across both runs. Missing
lane evidence fails the check.

Applicability: macOS ARM64 / GitHub.com host execution is the captured and reference
lane. Linux host replay is applicable in workspace CI; Linux live comparison and
all GHES versions remain **unverified**, not passing evidence. Docker/container
execution is not exercised here because no container behavior changes. This issue
does not claim the epic-wide platform or cross-feature acceptance matrix is closed.

## Step context identity (#98)

The source capture is the sanitized `job_message.json` acquired-message
fixture. It contains different wire UUIDs and `contextName` values, but its
shell step's context name is generated (`__run`). The named `build` shell
producer and consumer in `crates/execution/tests/step_context_identity_test.rs`
are derived from those captured step IDs and execute real Bash children. This
is a local production-step-loop regression, not a new named-step acquisition.
The `acquired_message_replay_keeps_wire_ids_through_job_completion` case also
retains the acquired envelope and runs it through `Runner::execute_job`, with
only its steps derived for the named producer and consumer.

| Scenario | Local check and expected observation | Evidence limit |
| --- | --- | --- |
| 98-S1 | Real Bash `$GITHUB_OUTPUT` and stdout `::set-output::` producer writes `hello` and `old`; a later shell prints `hello:old` from `steps.build.outputs`; `StepCompleted` retains the captured UUID. A generated `__run` has no expression entry. | A new named-step production capture and live GitHub.com/GHES reporting remain unverified. |
| 98-S2 | Condition-false named step records empty outputs and `skipped` outcome/conclusion; an `always()` shell reads both. Engine emits a skipped completion with the wire UUID. `crates/listener/src/tests/step_context_identity.rs` checks local Results Service and `complete_job` serialization against that UUID and conclusion value 7. | Backend acceptance and UI display remain unverified. |
| 98-S3 | A real failing Bash step with `continue-on-error` leaves `outcome=failure`, `conclusion=success`. A local Node pre/main/post action reports distinct IDs, retains the main output, and its post receives private `STATE_k`. | Live/reference stages and Linux host execution remain unverified. |
| 98-S3 nested | Checked-in nested composite actions invoke the same inner name with different inputs; only declared `first`/`second` outputs reach the parent. | Full composite expression semantics remain with #102. |
| 98-S4 | The producer-side expression value is exercised locally. | Acquired `jobOutputs`, completion output, and downstream `needs` cannot be accepted until #70 supplies the live wiring; no downstream success is claimed. |

The pinned reference source for expression visibility and result updates is
[actions/runner `ExecutionContext.cs` at cab9d1c](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/ExecutionContext.cs).
The issue-specific local checks are `cargo test -p execution --test
integration -- step_context_identity_test::` and `cargo test -p listener --lib
step_context_identity_test`. The eight execution tests and listener
serialization test passed locally on macOS 26.6.2 ARM64 with Bash 5.3.20.
The action manifest requests Node 20; the local regression seeds the installed
Node 26.9.0 binary into that cache slot to keep the test offline, so this is
not Node 20 runtime-parity evidence. The full `./tools/check.sh all` gate must
be recorded from the final source tree before marking the workspace lane green.
GitHub.com/GHES backend, the pinned reference-runner execution, and Linux/macOS
cross-host comparisons for #98 are **unverified**.

## Acquired run defaults (#71)

The source is GitHub.com's actual Run Service `acquirejob` body from the
[`defaults-run-71` capture run](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36091513712)
at revision `1e8954a0f87927ceeeb693167a59676fab3256b6`. The initial toolu
run failed at **Assert job defaults**, proving the old live path ignored the
payload. The sanitized `crates/execution/tests/defaults_run_job.json` retains
both ordered TemplateToken mappings, all eight step IDs, and the envelope;
27 credential, authorization, and mask values were replaced with deterministic
UUIDs. `crates/execution/tests/defaults_run_evidence.json` pins the raw and
sanitized hashes, workflow/action revisions, job ID, and run ID.
In the first matrix run at `4715b2b`, toolu completed all steps; the
GitHub-hosted job launched `/bin/sh` correctly but its raw `ps comm` value was
`/bin/sh` rather than `sh`. The final workflow compares executable basenames.

| Criterion / scenario | Captured input and exact observable result | Runnable check / current evidence |
| --- | --- | --- |
| AC-1, 71-S1 | Workflow `bash`/`defaults-71-workflow`, then job `sh`/`defaults-71-job`; real shell identifies `sh` and writes only `defaults-71-job/job.marker`. A capture-derived partial job layer clears the workflow shell. | `cargo test -p execution --test integration -- job_message_run_defaults_test::job_defaults_override_workflow` and `later_partial_mapping_clears_earlier_shell`; passing Linux production replay. |
| AC-2, 71-S1 | Captured explicit step uses Bash and `defaults-71-step`; `explicit.marker` exists there, not in the job directory. An empty explicit shell falls through to job `sh`. | `step_values_override_defaults`, `empty_explicit_shell_uses_job_shell`; passing Linux production replay. |
| AC-3, 71-S2 | A capture-derived absent-default job writes `root.marker` at the job workspace root. Relative job path, step path with spaces, and `${{ runner.temp }}` absolute path run in their selected directory. A nonexistent default cwd fails the correct step and its log names the full path. | `paths_and_absence`, `nonexistent_default_directory_fails_its_step_with_path`; passing Linux production replay. Host-shell fallback parity remains with #80. |
| AC-4, 71-S3 | The captured local composite writes `.defaults-71-composite.marker` at the workspace root; the next top-level `sh` step reads it from its job-default directory. | `composite_scope`; passing Linux production replay. Composite cwd coverage is recorded in the #81 section below. |
| AC-5, 71-S3 | Linux job-container shell, cwd, and container identity require #73/#80 integration and a Docker-capable Linux self-hosted runner. | **Unverified**; macOS job containers are not applicable to this epic. |
| AC-6 | The capture checker validates ordered wire types, synthetic credentials, fixture/workflow SHA-256, and recorded live runs against GitHub's jobs API. | `python3 scripts/test/defaults_run_capture_check.py --live` and `python3 -m unittest discover -s scripts/test -p 'test_defaults_run_capture_check.py'` pass. The full Linux `./tools/check.sh all` gate passed on the final source. |

The replay removes only the remote checkout step and copies the checked-in
local composite when needed. The captured absolute-path assertion is adjusted
to compare physical paths: macOS resolves `/var` to `/private/var` in `PWD`.
All default layers and step IDs come from the acquisition; named boundary
transformations are identified in the test. The pinned reference contract is
[actions/runner `JobExtension.cs` at cab9d1c](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/JobExtension.cs)
and its top-level/composite scope rule in
[`ScriptHandler.cs`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/Handlers/ScriptHandler.cs).
The [live macOS matrix run](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36095548084)
passed both `defaults-toolu` and `defaults-reference` at revision `48d6386`,
with every issue 71 assertion green. Both jobs used the same workflow and
composite-action revision. `actions/checkout@v4` emits a Node 20 deprecation
warning on the GitHub-hosted lane. A later optional trial of `checkout@v5`
([run 36096230974](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36096230974))
passed the hosted lane, but its Node 24 child on this macOS toolu host remained
in `_dyld_start` before action code ran; that trial was cancelled, and the
workflow retained the fully passing `checkout@v4` revision. Linux host, GHES, container, and full absent-shell
comparisons remain **unverified** until their respective runners or dependency
issues are available.
## Broker control messages (#77)

The checked-in V2 broker job and migration envelopes are synthetic,
capture-shaped fixtures. Tests change only their wire type (and, for cursor
ordering, message ID) to exercise the production parser and listener
classifier. They do not claim an unknown control was captured from GitHub.

| Scenario | Checked behavior | Evidence limit |
| --- | --- | --- |
| 77-S1 | `message_types`, `broker_poll_parser`, and `broker_control` preserve a future type's original envelope and choose the unknown route without decrypting an opaque body. | An actual unknown broker response, subsequent live poll cursor, and following job acquisition remain unverified. |
| 77-S2 | `broker_control` checks repeated/older IDs against the listener cursor rule. Existing `early_ack` coverage retains job-only acknowledgement. | Broker redelivery and acknowledgement failure against GitHub remain unverified. |
| 77-S3 | Production refresh signs a JIT assertion, bounds transient exchange attempts, and publishes only a nonempty successful token. `cargo check -p listener --lib --tests` verifies code integration. | No real `ForceTokenRefresh` message or OAuth fault sequence is available; live rotation, retry and cancellation remain unverified. |
| 77-S4 | Capture-derived type variants distinguish `RunnerRefresh`, `AgentRefresh`, `RunnerRefreshConfig`, `HostedRunnerShutdown`, and future names. | Live control-message injection and the pinned reference-runner comparison remain unverified. |

The full gate is `./tools/check.sh all`. On this macOS host, test binaries
have stalled in `_dyld_start` before Rust test code runs; GitHub Linux `ci`
and `ci-macos` are the authorized gate evidence for this branch. Actual
GitHub.com and GHES control-message behavior remains unverified.

## Post-action cleanup (#101)

`crates/execution/tests/post_results_test.rs` replays the sanitized acquired
message from #68 through the real `Runner::execute_job` path, using a committed
Node action fixture. It checks successful mains plus failed posts, LIFO order,
state/input/action-name isolation, hard missing-script diagnostics with later
posts continuing, changing `post-if` status, and cancellation during an
observed running post. `crates/listener/src/tests/post_results.rs` consumes the
real event stream through `StepCollector` and serializes the actual
`CompleteJobRequest`, checking separate main/post results and failed job status.
`execution::post_drain::tests::cancel_budget_is_shared` checks that later posts
use the remaining time on a single deadline. The exact local pass/fail evidence
is the repository gate and issue-specific test commands, recorded with the worker report.

| Issue scenario | Exact local observation required | Test and runnable check | Evidence status |
| --- | --- | --- | --- |
| S1 / failed post result | Main `Success`, distinct post `Failure`, job `Failure`; completion JSON uses conclusion `3`, main `2`, post `3`, number `3`, and `Post Workflow input passed to action`. | `post_failure_reaches_job_completed`; `post_results_reach_distinct_completion_records`; `cargo test -p execution --test integration -- post_results_test::post_failure_reaches_job_completed && cargo test -p listener --lib post_results_reach_distinct_completion_records` | Local production replay; GitHub UI unverified. |
| S2 / hard error drain | A/B mains then B missing-script failure followed by `A:post` with A-state and A input; four completion records, final Failure. | `hard_post_error_keeps_draining_lifo`; `cargo test -p execution --test integration -- post_results_test::hard_post_error_keeps_draining_lifo` | Local production replay. |
| S3 / live conditions | After B post failure, A `failure()` runs and A `success()` skips. After A main failure, B `failure()` runs and B `success()` skips. A hard B main error still completes after A `failure()` cleanup. Cancelling a running main keeps `Cancelled` even when A's eligible post fails. | `post_conditions_follow_live_status`, `hard_main_error_still_completes_after_post_cleanup`, `cancelled_main_keeps_failure_post_and_final_cancelled`; `cargo test -p execution --test integration -- post_results_test::post_conditions_follow_live_status && cargo test -p execution --test integration -- post_results_test::hard_main_error_still_completes_after_post_cleanup && cargo test -p execution --test integration -- post_results_test::cancelled_main_keeps_failure_post_and_final_cancelled` | Local production replay; pinned reference unverified. |
| S4 / state and report isolation | B post reads B-state/B/`__self_2` before A reads A-state/A/`__self`; four distinct report IDs. | `repeated_posts_keep_state_and_report_identity`; `cargo test -p execution --test integration -- post_results_test::repeated_posts_keep_state_and_report_identity` | Repeated top-level local production replay; nested pending #102. |
| S5 / cancellation budget | B's observed post start precedes cancel; A's later `cancelled()` post finishes, B never writes a late finish marker, job `Cancelled`, completion within 5 s. A 120 ms shared deadline reaches zero after elapsed time. | `cancelled_posts_retest_always_and_complete_job`, `cancel_budget_is_shared`; `cargo test -p execution --test integration -- post_results_test::cancelled_posts_retest_always_and_complete_job && cargo test -p execution --lib cancel_budget_is_shared` | Local real processes plus deadline unit check; multi-post expiry unverified. |

These tests use the #68 sanitized acquired message and real Node 26.9.0 on
macOS ARM64, with local action files preseeded at its captured workspace path.
They preserve the captured job IDs, context names and wire token types; the
action paths and manifest defaults are labelled test transformations.

The #101 repeated-instance check covers two top-level local actions. Issue
#102 adds a captured-job replay with nested Node pre/main/post stages and
checks LIFO order, distinct report IDs, and instance-private `STATE_*` values.
The cancellation process test starts a slow post, cancels after its observed
start marker, checks a second slow `cancelled()` post runs, and verifies the
first process produces no late marker. The multi-slow-post expiry case is
covered only by the shared-deadline unit check; end-to-end expiry remains
unverified until a short test deadline can be injected into the production
drain without changing the runtime default.

The committed `post-results-live.yml` and matching action allow the same
revision to be dispatched to toolu and the pinned official runner using
different runner labels. Its failure scenario expects both mains to succeed,
the second post to fail, and the job to fail with a separate post record. Its
cancel scenario must be cancelled externally after the long post begins.
The reference source pin is `actions/runner`
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`.

GitHub currently reports zero registered runners for this repository, so the
GitHub.com UI, official-runner comparison, Linux host execution, and GHES
lanes are **unverified**. Local replay on macOS ARM64 tests the execution and
report serialization paths, but does not claim remote acceptance.

## Authenticated action downloads (#76)

The acquired job fixture `crates/execution/tests/defaults_run_job.json` came
from [GitHub.com run 36091513712](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36091513712).
Its Launch URL, API host, plan and job identifiers, action ref, and sanitized
tokens drive `crates/execution/tests/action_downloads_test.rs`. The
[pinned runner source](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/ActionManager.cs)
provides the Launch and legacy GHES request contracts. The same fixture's
provenance and hash are in `crates/execution/tests/defaults_run_evidence.json`.

| Scenario | Current evidence | Evidence still needed |
| --- | --- | --- |
| 76-S1 | The captured `actions/checkout@v4` job selects its exact Launch path and sends `{action,version}`. Production prefetch and step execution share the acquired-job resolver and a revision-qualified cache. | A sanitized real Launch response and public/private/internal SHA, tag, branch, subpath, and local-action live runs. |
| 76-S2 | A capture-derived host variant selects its GHES API URL; legacy discovery uses the action-download resource from connection data. | A real GHES and GitHub Connect job, including the returned cross-host archive URL, plus an older-server fallback run. |
| 76-S3 | Local HTTP failure and redirect probes check 401/403 propagation, Basic archive auth on the source origin, and no auth at a different origin. | Live token expiry, rate limiting, transient failure, retry, and durable log redaction observations. |
| 76-S4 | Existing downloader tests cover streaming extraction, corrupt archive cleanup and concurrent staging; the new parser rejects traversal and the fetcher keys by resolved SHA. | Live moved-ref, cancellation, and same-revision concurrent execution observations. |

Run `cargo test -p execution --test integration -- action_downloads_test::` and
`./tools/check.sh all`. Local newly built test binaries on the development
macOS host have stalled in `_dyld_start` before Rust code executes. The
GitHub Linux `ci` and `ci-macos` PR checks are the authorized gate evidence
for this branch. The missing live lanes above remain **unverified**.

## Composite expression and scope parity (#102)

`crates/execution/tests/composite_semantics_test.rs` replays the sanitized real
#68 GitHub.com acquisition through `Runner::execute_job`. Its parent wire ID,
`contextName`, and type-3 `with.who` expression token are preserved; the local
action path and optional final script body are the labelled test
transformations. The checked-in `composite-102-*` and `node-102-*` actions run
real Bash and Node stages. `composite_bounds_test.rs` invokes the production
composite executor with the committed sleeper action and a short fixed
deadline. The capture and all fixture hashes are checked by
`python3 scripts/test/composite_semantics_evidence_check.py crates/execution/tests/composite_semantics_evidence.json`.

| AC / scenario | Exact expected observation | Test / runnable check | Current evidence |
| --- | --- | --- | --- |
| AC-1 / S1 | `format`, `contains`, bracket access, shell expression, `github.action_path` in step `env`, and output mapping produce `hello world`, `world/42`; malformed syntax and forbidden `secrets.*` emit a parent error instead of an empty string. | `expressions_render_functions_brackets_conditions_and_outputs`, `malformed_run_expression_fails_step_then_runs_cleanup`, `forbidden_context_fails_visibly_then_runs_cleanup`; `cargo test -p execution --test integration -- composite_semantics_test::` | Captured-job macOS replay passes; expression-valued composite cwd is covered by the #81 replay below. |
| AC-2 / S2, S4 | Exit 1 without continuation writes `fail`, `failure-cleanup`, `always-cleanup` and ends Failure. With continuation it writes `fail`, `ordinary`, `always-cleanup`, exposes `failure/success` and ends Success. A malformed `if` stops the loop; a hard nested error runs cleanup with a parent-scoped `##[error]`. | `conditions_run_failure_and_always_cleanup_after_inner_exit_one`, `continue_on_error_keeps_raw_failure_and_runs_ordinary_and_always`, `hard_nested_error_keeps_parent_attribution_and_runs_failure_cleanup`, `malformed_if_expression_stops_composite_loop`; same test binary | Captured-job macOS replay passes; GitHub UI pending. |
| AC-3 / S3 | Reusing child input/step names exports `world-one/world-two`; parent still reads `world/success/success/` with no child `steps.first` leakage. Nested and parent `github.action_path` each match their own action directory. | `nested_repeated_names_keep_distinct_inputs_and_outputs`; same test binary | Captured-job macOS replay passes. |
| AC-4 / S5 | `GITHUB_ENV` and `GITHUB_PATH` reach later inner and outer steps; `LOCAL_ONLY` does not. Only declared output `result=42` escapes. Two nested Node posts log `STATE_POST=two`, then `STATE_POST=one`, under distinct successful report IDs. A nested `post-if: failure()` sees a later global job failure. | `file_commands_step_env_and_nested_posts_are_scoped`, `nested_post_if_failure_sees_later_global_job_failure`; same test binary | Real Bash/Node macOS replay passes; remote post records pending. |
| AC-5 / S4, S5 | An observed-start cancellation returns Cancelled, skips ordinary work, writes `always()` cleanup, and never writes `late`. A short parent deadline returns Failure and prevents `late`/ordinary work. On both paths the Bash and descendant `sleep` PIDs are no longer live; a blocked action-resolution operation also obeys the parent deadline or cancellation. Inner logs and hard errors use the captured parent ID. | `observed_start_cancel_kills_child_and_runs_always_cleanup`, `parent_deadline_kills_real_composite_child_without_late_work`, `blocked_resolution_obeys_parent_deadline`, `blocked_resolution_obeys_cancellation`; `./tools/check.sh all` | Captured-job cancel, real short-deadline subprocess and blocked event-send resolution pass locally; composite cwd awaits #81. |

The committed [parity workflow](../.github/workflows/composite-semantics-102.yml)
runs identical action files on macOS toolu and GitHub-hosted macOS/Linux lanes.
`--require-live` on the evidence checker refuses to call missing runs passing.
GitHub currently reports zero registered self-hosted runners; its toolu lane,
both hosted reference runs, remote log/timeline verification, and GHES are
**unverified** until run URLs and runner identities are recorded in the
evidence JSON. GitHub-hosted runners may differ from the source pin
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`; their actual binary version
must be recorded before claiming pinned-reference parity. A Linux toolu host
and a GHES server are unavailable here, so those applicable lanes are also
**unverified**. Issue #81 remains open and owns composite cwd parsing and
resolution; #102 must consume and test its expression site after it lands.

## Step process CI flags (#72)

The [captured GitHub.com job](../crates/execution/tests/incoming_contexts_matrix_0.json)
from #68 and the [captured Linux job-container message](../crates/toolu-runner/tests/fixtures/job_container_message.json)
from #73 retain their wire IDs and template-token types. The [evidence record](../crates/execution/tests/step_process_ci_evidence.json)
pins their hashes, the committed [probe actions](../crates/toolu-runner/tests/fixtures/local_actions/ci-72-node/action.yml),
the [parity workflow](../.github/workflows/step-process-ci-72.yml), and the
current status of each live lane. The production replay executes real Bash,
Node pre/main/post, and nested composite shells; process stdout is the oracle.

| Acceptance / scenario | Exact process result | Proof and applicability |
| --- | --- | --- |
| AC-1 / 72-S1 | With no `CI`, a child prints `true`; runner `CI=false` remains `false`; captured job `CI=job` and step `CI=step` win in order; a step's empty value remains empty. | `env -u CI cargo test -p execution --test integration -- step_process_ci_test::` and `CI=false cargo test -p execution --test integration -- step_process_ci_test::`; `host_script_defaults_and_preserves_ci_precedence`, macOS ARM64 host replay. |
| AC-2 / 72-S2 | Attempts to set `GITHUB_ACTIONS=false` at job, step, or container declaration still print `true` from the child. | Same host replay plus `bash scripts/test/step_process_ci_linux.sh`; real Docker on Linux ARM64. |
| AC-3 / 72-S3 | Node pre/main/post each print empty `CI` and `true`; the post retains its originating step value after a later `GITHUB_ENV` write. Nested composite inner and parent shells print the same pair. Nested Node posts retain both their own step `CI` and an inherited parent-step `CI`. A malformed `CI` expression visibly fails before starting an action child. | `node_pre_main_post_keep_originating_empty_step_ci`, `composite_shell_nested_empty_ci_and_forced_github_actions`, `nested_node_post_keeps_its_composite_step_ci`, `nested_node_post_keeps_inherited_parent_step_ci`, `malformed_action_ci_expression_fails_visibly`; both isolated runner-CI modes pass. |
| AC-4 / 72-S3 container | Container shell and verification shell retain declaration `CI=false`; Node stages and nested composite step override it with empty `CI`; all print `GITHUB_ACTIONS=true` and hostname `container-73-probe`. Absent declaration uses runner `CI` or `true`. | `job_container_ci_test.rs` has two Linux-only ignored-by-default tests; the explicit wrapper above requires Linux and a real Docker socket and runs both tests with absent, false, and distinct `runner` values for runner `CI`. A Darwin zero-test result does not count. |
| AC-5 | `./tools/check.sh all` must pass without suppressions. | Full gate result is recorded in the evidence JSON and PR verification. |

The [GitHub-hosted workflow run](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36184411746)
passed the committed process assertions on macOS, Linux, and a Linux job
container at revision `60ad9fb`. Matching self-hosted toolu lanes require
registered runners; the GitHub API currently reports zero. The
[evidence checker](../scripts/test/step_process_ci_evidence_check.py) verifies
capture/file hashes and any recorded GitHub run IDs; `--require-live` fails on
missing lanes. GitHub-hosted runner binaries have not been shown to equal the
epic's pinned official source revision `cab9d1c`, so hosted runs alone do not
establish pinned-reference parity. Toolu GitHub.com acquisition, GHES, and the
Docker-action half of 72-S3 remain **unverified**; Docker actions belong to
open #75. macOS job containers are not applicable because this runner supports
them on Linux only.

## Job containers (#73)

Local component evidence uses real Docker output and actual shell/action code.
`job_container_test.rs` and `job_container_linux_test.rs` drive `Runner::execute_job`
using the existing message fixture with explicit test steps/declarations. This is
production execution coverage, **not** an acquired container-message capture.
No parser or constructed fixture result establishes live backend parity.

| Scenario | Test surface and exact observable | Status |
| --- | --- | --- |
| 73-S1 | `execution/tests/job_container_linux_test.rs`: Ubuntu OS, shared hostname across shell/Node/composite/posts, container cwd and exact marker bytes | Local Linux lane; see command below |
| 73-S2 | `docker/tests/job_container.rs`: paths with spaces, metadata equals inspect, ephemeral published port, user bind-volume persistence, multiline opaque env preserved, cleanup; `job_container_composite_test.rs`: container shell, expression paths and ENV/PATH; Linux lane: OUTPUT/ENV/PATH/STATE | Local Docker plus live checkout/artifact byte verification (runs below) |
| 73-S3 | Service aliases and Docker actions sharing the job network | Unverified; requires #74/#75 integration |
| 73-S4 | `docker/tests/job_container_failures.rs`: observed-start cancellation, timeout/post exec, missing daemon, pull/create/start/exec failure, owned-resource removal | Explicit real-Docker lane |
| 73-S5 | `execution/tests/job_container_test.rs`: non-Linux declaration fails before host workspace/step, absent declaration keeps host behavior | Default macOS lane; Linux setup-cancel conclusion regression |
| Live applicability | Captured `jobContainer` replay; toolu 36057599244 / official 2.337.0 36057648598: six markers, exact artifact bytes and cleanup | GitHub.com paired harness passed; GHES remains unverified |

Ordinary repository gate: `./tools/check.sh all`. Resource-creating Docker tests
are ignored there and must be run explicitly. With a local Linux daemon:

```sh
TOOLU_CONTAINER_TEST_ROOT=/absolute/shared/test-root \
  cargo nextest run -p execution -j 1 -E 'test(job_container) | test(container_spec) | test(container_options) | test(container_create_options)' --run-ignored all
```

The Linux production test is compiled only on Linux. On macOS, the component
adapter can exercise a Linux daemon, while production rejects container jobs.
For a VM daemon, set `TOOLU_CONTAINER_TEST_ROOT` to a directory shared at the same
absolute path by the test process and daemon. Missing Docker/sharing fails the
explicit lane; it never counts as a pass. Tests pin Ubuntu image digest
`sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3`.

On 2026-09-24 the initial macOS explicit lane passed all seven tests. After
review fixes, the expanded Linux lane passed all 22 selected tests on the ARM64
Colima Docker daemon, including the exact live action fixtures, setup cancellation,
composite action_path in env, exact workspace roots and host completion-hook PATH. Linux
`cargo clippy -p execution --all-targets -- -D warnings` also passed. Run
resource-snapshot tests serially (`-j 1`) on an otherwise idle test daemon.
The real-Docker lane also requires the Docker CLI for independent inspect checks.
The branch-only `multistep-live.yml` workflow requires a dedicated runner label
and pins its Ubuntu image, checkout and artifact action revisions. It exercises
checkout, shell, Node pre/main/post, composite, command files and artifact bytes;
its presence alone does not establish live success.

The opt-in `toolu-runner/tests/job_container_live.rs` harness targets only
`Falconiere/toolu-ghrunner`, this feature branch and `multistep-live.yml`.
It requires `GH_TOKEN`/`GITHUB_TOKEN`, two separately provisioned, distinct
`toolu-73-*` labels in `TOOLU_CONTAINER_TOOLU_LABEL` and
`TOOLU_CONTAINER_REFERENCE_LABEL`, and access to their Docker daemon. It
compares successful runs at one workflow SHA, stage markers, exact downloaded
artifact bytes and removal of the artifact-recorded container/network IDs.
It does not provision/authenticate the reference runner, capture/sanitize an
acquired message, replay a capture, or cover GHES. Compilation or an ordinary
ignored result is not live evidence.

```sh
cargo nextest run -p toolu-runner --features live --test integration \
  --run-ignored only -j 1 job_container_live::
```

The real GitHub.com capture is documented in
`crates/toolu-runner/tests/fixtures/job_container_message.md`. Its Linux replay
(`execution/tests/job_container_capture_replay.rs`) preserves captured step UUIDs,
context names and token shapes, removes only checkout/artifact backend steps, and
executes the original shell, pinned Node probe, local composite and final output
verification. It exposed and now pins local `self` references carried only in
`reference.path`, and named `steps` contexts distinct from internal step UUIDs.
The three captured parse/replay checks passed on Linux with real Docker.
The live comparison reads GitHub's combined job log, which preserves all Node
stages even when per-step log metadata reuses the main step ID for posts.

On 2026-09-24 the paired harness passed (67.337 s) against
[toolu run 36057599244](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36057599244)
and [official run 36057648598](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36057648598).
Both used workflow SHA `44aedf38e95137ad6128a14db84475e8048cd791` and the
same pinned action revisions. The toolu binary included the captured local-reference
and context-name fixes in this change; workflow revision and runner revision are
distinct. The harness verified all six markers, exact `container-73-artifact\n`
bytes, and absence of each artifact-recorded container/network. Official logs
contain terminal escapes; the harness permits these only in private captured
output. The earlier official log-fetch failure was CLI output protection, not
a failed workflow or delayed publication. Services/Docker-action integration
(#74/#75), private-registry authentication failure, and GHES remain unverified.

After rebasing onto #68 (`aa674e9`), the Linux regression lane passed the
seven acquired-context/real-Node cases, eight action-reference cases, and all
three captured-container parse/replay cases. The standalone container routing
probe initially exposed its constructed `self` references using `name` rather
than the captured `path` field. Correcting those two references made the same
probe pass (12.490 s), with all identity/path/output/post/cleanup assertions
unchanged. Incoming contexts and fallible Node input evaluation are retained
alongside container translation. The live-run links above describe the earlier
workflow/source revisions; this rebase evidence is local Linux replay.

## Job outputs from the acquired message (#70)

`crates/execution/tests/job_outputs_message.json` is a sanitized Run Service
`acquirejob` body from [run 36173987791](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36173987791)
at `5b65420157885949e57c3daea4021e074049baee`. It retains the real
`jobOutputs` token, step UUID, distinct `contextName`, and context shape. Its
27 credential fields are deterministic synthetic UUIDv5 replacements. The
fixture hash and replacement derivation are checked by
`python3 scripts/test/job_outputs_evidence_check.py --capture`.

| Scenario | Production-path check | Live GitHub.com observation |
| --- | --- | --- |
| 70-S1: real step output reaches `needs` | `execution/tests/job_outputs_test.rs::captured_output_is_evaluated_after_real_github_output_step`; `listener/src/tests/job_outputs.rs` carries the final map; `wire/tests/job_outputs_test.rs` pins `CompleteJobRequest.outputs` as `{value: string}` | Toolu and official producers plus dependent consumers passed, checking `hello-output`. |
| 70-S2: empty, multiline, Unicode, skip, failure, cancellation | `empty_multiline_and_unicode_values_follow_github_output_file`, `skipped_producer_step_omits_its_job_output`, `failed_step_keeps_output_and_later_expression_error_keeps_prior_output`, and `cancelled_job_evaluates_output_written_before_cleanup` in the captured-message engine test | Both normal consumers checked empty, multiline, and Unicode values. Skipped/failed/cancelled producers have local replay coverage only; live parity for those paths remains unverified. |
| 70-S3: secret-containing output is omitted | `message_secret_and_dynamic_add_mask_are_skipped_with_warnings` checks omission of a message secret and `::add-mask::` value, with warning events | Both consumers checked absence of the dynamically masked output. Message-secret and post-registered-mask cases have no live comparison. |
| 70-S4: matrix cells and post-stage finalization | `captured_matrix_and_strategy_are_visible_at_job_output_time`, `job_output_hash_waits_for_real_node_post_step`, and `post_action_env_is_available_to_final_job_output` in the captured-message engine test | Each lane ran `alpha` and `beta` producers; the dependent consumer checked both aggregated keys. Both normal consumers checked `POST_VALUE=after-post`, set by the Node post action. |

[Paired run 36184896428](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36184896428)
completed with all ten jobs successful at runner revision
`1b37a9cc7102effda467297fcdb32b55e4711676`. The toolu macOS JIT runner
was `toolu-70-final`; the official macOS JIT runner was `official-70`, from
actions/runner v2.337.0 binary commit
`397b032cbf865e9c3ddfab89d533ec19325e1273` (asset SHA-256
`5a2cd92908a93d7276a194e1de6008099f3e7946f3f8e14aa7a1a7b4a31fdec2`).
Both lanes used the same workflow revision and checkout pin
`11d5960a326750d5838078e36cf38b85af677262`. The runner-specific jobs
executed on the named JIT runners; downstream assertions ran on GitHub-hosted
Ubuntu. `python3 scripts/test/job_outputs_evidence_check.py --live` verifies
the run/job conclusions and runner identities against GitHub, plus the recorded
workflow/action hashes. Full run metadata is in
`crates/execution/tests/job_outputs_evidence.json`.

The local macOS `./tools/check.sh all` reached cargo tests after fmt, clippy,
and guardrails passed, then stalled at the known host `_dyld_start` issue and
was stopped under the orchestrator's instruction. GitHub Linux `ci` and
`ci-macos` are the full gate for this PR. A live Linux toolu host and GHES were
unavailable, so those lanes remain unverified.

# Run Service annotations (#82)

The #82 replay starts with the sanitized captured acquired job in
`crates/execution/tests/incoming_contexts_matrix_0.json`. The shell probe
prints real workflow commands; the listener replay also executes a local
composite action. Its recording HTTP receiver inspects the actual
`completejob` POST through the production transport. The local host is macOS
arm64; the capture retains real step UUIDs, context names and token shapes.

| Scenario | Check and observable | Evidence status |
| --- | --- | --- |
| 82-S1 | `cargo nextest run -p execution --test integration command_annotation_test::`: escaped error, warning, notice, title/path/range, mask | Passed locally; payload checked below |
| 82-S2 | Same probe: absent/end-only/invalid/descending/multiline ranges and blank message | Passed locally; upstream range normalization and omission checked |
| 82-S3 | `cargo nextest run -p listener -E 'test(annotation_reporting) | test(watchdog_trip)'`: step results carry numeric 3/2/1 annotations and nested composite events use the enclosing step number; outage is job-level | Passed locally against the outgoing Run Service POST |
| Journal and command compatibility | `cargo nextest run -p toolu-runner --test integration -- journal_types_test::` and `-- gh_compat_commands::` (same `integration` target) | Passed locally; journal v1 shape retained |
| Gate | `./tools/check.sh all` | Passed locally on 2026-09-25; formatting, clippy, guardrails and workspace tests green |

The branch-scoped [live workflow](../.github/workflows/annotation-82-live.yml)
uses `toolu-82-tool` and `toolu-82-reference` self-hosted labels on one push
SHA. Set the repository variable `TOOLU_ANNOTATION_82_LIVE=true` after both
runners are provisioned to enable the jobs; skipped jobs are not live evidence.
Its ignored verifier `crates/toolu-runner/tests/annotation_live.rs`
requires `GH_TOKEN`, both runner names, and expected binary versions as
`TOOLU_ANNOTATION_{TOOLU,REFERENCE}_{NAME,VERSION}`. The GHES lane also needs
`TOOLU_ANNOTATION_GHES_URL`, `_TOKEN`, `_REPO`, `_BRANCH`, and the corresponding
`_TOOLU_NAME`, `_REFERENCE_NAME`, `_TOOLU_VERSION`, `_REFERENCE_VERSION` values.
The verifier fails if the run, runner identity/version, matching OS and
architecture labels, or Checks API annotations are missing or different.
GitHub's Checks annotation API does not
carry a step number; inspect the Checks UI for shell versus composite step
association and record the run/check URLs, host OS/architecture, GitHub or
GHES version, workflow SHA, runner revisions and a UI capture before marking
a live lane verified. The selected live test checks that both named runners
are online with their fixed labels and the same host platform before waiting
for the branch run.

| Live lane | Checks API | Checks UI and step association | Host/version proof |
| --- | --- | --- | --- |
| GitHub.com macOS toolu + official | Unverified: a 2026-09-25 repository API check found zero registered runners | Unverified | Unverified |
| GitHub.com Linux toolu + official | Unverified: no paired online #82 runners provisioned | Unverified | Unverified |
| Supported GHES toolu + official | Unverified: no GHES endpoint/token or paired runners configured | Unverified | Unverified |
## Issue #100 — conditional cleanup

Source: sanitized #68 acquisition `crates/execution/tests/incoming_contexts_matrix_0.json`.
`conditional_cleanup_test.rs` substitutes real shell/action probes into its tokens,
retains wire UUIDs, and changes the first generated contextName to `probe` so its
raw/effective results can be read. It uses the production `Runner` job path.
The policy oracle is static inspection of official runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5/StepsRunner.cs`; that is **not** reference execution.

| AC / scenario | Exact test / observable evidence |
| --- | --- |
| AC-1, AC-5 / S1 | `execution_errors_follow_exit_failure_policy_and_continue_on_error`: exit 7, nonexistent cwd, missing Bash via PATH and missing local action; both continuation settings. Raw failure/effective success or failure, exact following markers, one completion per wire UUID. `condition_and_env_errors_fail_without_continue_adjustment_and_allow_cleanup` distinguishes pre-execution evaluation failures. `workspace_setup_failure_never_runs_user_steps_and_completes_once` uses a real file blocking workspace creation. |
| AC-2 / S2 | Same error matrix and `real_step_timeout_is_failure_then_conditional_cleanup_runs`: actual one-minute timeout of sleep 300; failure/always/!cancelled markers present, ordinary/cancelled absent, final Failure. |
| AC-3 / S3 | `observed_start_cancel_retests_current_condition_and_runs_cleanup`: cancel after actual stdout `started`, never a fixed sleep; current success()/!cancelled() interrupt, always() finishes after release; later always/cancelled markers and final Cancelled. |
| AC-3, AC-4 / S4 | `cancellation_before_first_step_runs_only_eligible_cleanup`; `cancellation_during_real_action_resolution_runs_later_cleanup` waits for an actual HTTP request to a deliberately unresponsive loopback resolver (no fabricated successful service response); `shutdown_interrupts_always_and_skips_later_user_steps` exercises the independent shutdown API. Actual CLI SIGTERM and a deterministic between-step/timeout race remain unverified. |
| AC-4, AC-5 / S5 | `cancellation_kills_owned_child_and_grandchild_but_not_unrelated_process`: real Bash/sh/sleep family; ps confirms death, independently spawned sleep survives. `job_cancellation::tests::cleanup_processes_share_one_deadline` uses real processes with a shortened shared deadline. `post_results_test` retains LIFO, hard-error and state/report identity checks; running always() posts now finish on graceful cancellation. Container/service-resource comparisons remain required. |

Run `cargo test -p execution --test integration -- conditional_cleanup_test:: post_results_test::`
and the unchanged `./tools/check.sh all`. The initial macOS ARM64 replay reproduced
missing-cwd early termination and pre-cancel cleanup omission. The first eight
regressions subsequently passed, including the real 60-second timeout; composite
bounds/semantics regressions also passed. Final gate and additional verification
status must be recorded from the current revision before delivery.

On 2026-09-25 the unchanged `./tools/check.sh all` passed in the Linux ARM64
`toolu-72-gate-tools:local` container against an exact source snapshot of this
worktree: 981 tests passed, zero failed, 25 were ignored by the existing suite.
Log: `/tmp/issue100-linux-gate3.log`. All nine conditional-cleanup cases, the
shared-deadline unit test and six `gh_compat_prepost` cases passed. The latter's
old escaped-Err expectation was updated to exact `Ok(Failure)` while retaining
the real Node post-state assertion. The macOS full gate was stopped after a test
binary was sampled stalled in `_dyld_start`; targeted macOS tests passed, but
that incomplete full run is not a pass. None of this certifies ignored/live lanes.

`conditional_cleanup_evidence.json` records scenario mappings and outstanding
lanes. `python3 scripts/test/conditional_cleanup_evidence_check.py --local` validates
mapping only. Without `--local`, missing required lane evidence fails explicitly.
Linux Docker resources, real CLI SIGTERM, GitHub.com UI/backend conclusions,
pinned reference comparisons and GHES are **unverified** until their evidence is
recorded. Supported GHES needs a supplied server/version/credentials; none is
configured for this worker. Static source review, ignored tests, and local replay
cannot close those requirements.

Available live Docker validation (2026-09-25): the four ignored
`execution::docker::job_container_failures` cases were explicitly run on Linux
ARM64 against the real Colima daemon with shared `TOOLU_CONTAINER_TEST_ROOT`,
`DOCKER_HOST=unix:///var/run/docker.sock`, and the fixture's pinned Ubuntu digest.
Command: `cargo test -p execution --lib docker::job_container_failures -- --ignored --test-threads=1`.
All four passed: observed-start cancellation and timeout keep the container for
post execution, then remove its container/network; missing executable and real
pull/create/start failures leave no owned resources. Log:
`/tmp/issue100-docker-resources2.log`. The first attempt never exercised the runner
because the image lacked Docker CLI; installing `docker-cli` resolved that setup
error. Full mixed-service/sentinel and pinned-reference S5 comparisons remain
unverified.

The orchestrator confirms no GHES endpoint/credential exists for this epic run.
A fresh repository runner API check found only offline `toolu-70-final`, with no
online paired issue-100 toolu/official runner. Consequently acquired-job SIGTERM,
GitHub.com backend/UI comparison, and pinned-reference/GHES lanes are unverified.
Per explicit orchestrator direction these gaps do not block PR delivery; the
Linux gate and GitHub `ci`/`ci-macos` determine code delivery readiness.


## Service containers (issue #74)

Production replay is in `crates/execution/tests/service_containers_test.rs`.
It reads the captured #73 GitHub.com envelope at
`crates/toolu-runner/tests/fixtures/job_container_message.json`, preserving its
wire types/IDs and explicitly replacing steps/container declarations with service
probes. **These service tokens are replay variations, not a newly acquired
service payload.** The source capture's provenance remains in its sibling `.md`.
Real Docker nginx responses, inspect results, health commands and an authenticated
local registry are the oracles; no successful service or registry response is
mocked. nginx is `1.27-alpine` (observed manifest
`sha256:65645c7bb6a0661892a8b03b89d0743208a18dd2f3f17a54ef4b76fb8e2f2a10`).

Run `bash scripts/test/service_containers_linux.sh` on a Docker host. It builds
inside Linux, shares the daemon, provisions a disposable htpasswd-authenticated
`registry:2`, pushes an nginx fixture, and checks actual production replay.
The optional `TOOLU_SERVICE_TEST_ROOT` selects the daemon-visible mount root;
`TOOLU_SERVICE_TEST_IMAGE` selects a Rust image (default `rust:1.94.1`). Registry
credentials are disposable local test values only. macOS production rejects
nonempty services, even when its Docker daemon is Linux.

| AC / scenario | Test and exact observable result | Evidence / applicability |
| --- | --- | --- |
| AC-1 / 74-S5 | `services_reject_unsupported_host_before_user_steps`: macOS completion Failure, no StepStarted or marker. `malformed_services_fail_before_user_steps` rejects a scalar mapping; `empty_services_do_not_require_docker` writes exact marker. | Passed on Linux ARM64 and macOS ARM64; macOS rejects before user steps. |
| AC-2 / 74-S1 | `services_host_dynamic_port_connects_and_cleans_up`: curl uses inspected expression port and receives nginx HTML; container/network inspect returns 404. `services_container_dns_connects_on_shared_network`: Alpine wget reaches `web`, network matches job container, both containers/network gone. | Passed on real Linux ARM64 Docker 29.5.2. |
| AC-3 / 74-S2 | `services_unhealthy_prevents_steps_and_retains_logs`, `services_delayed_health_and_no_healthcheck_permit_steps`: false fails without steps and retains nginx logs; delayed success and disabled check permit steps. `docker::service_health::tests::never_healthy_service_respects_budget` uses a real starting container and a short explicit helper budget. | Passed; production budget is 300s, helper test budget 150ms. No claim of exact upstream timing parity. |
| AC-4 / 74-S3 | `services_multiple_evaluated_env_volumes_and_unrelated_resources_survive`: real health command asserts evaluated env, bind-file bytes and hostname for two services; both respond. `services_fixed_port_conflict_is_actionable`: real listener forces Docker start error. `services_private_registry_auth_and_masked_credentials`: correct auth pulls/runs, wrong auth fails before steps; registered credential output is `***`. | Passed using actual registry:2 auth and daemon pulls, not mocked HTTP. |
| AC-5 / 74-S4 | `services_second_start_failure_cleans_first`, `services_cancellation_during_health_and_job_cleans_resources`: observed service/step triggers cause cancellation, conclusion Cancelled and inspect 404; failed second image removes first. Multiple-service test preserves an unrelated container, its network and the bind file. | Passed on Linux ARM64; cancellation is event-triggered, not timing-only. |
| AC-6 / gate | `./tools/check.sh all`; health option parser/typed-request tests live in docker/tests. | Passed on macOS ARM64 (995 tests) and Linux ARM64 (993 tests); ignored Docker/live cases are not counted as passes. |

On 2026-09-25 the Linux real-Docker lane passed 11 production replay tests,
four health-option tests, and the never-healthy timeout test (16 passed, zero ignored in this explicit lane). Both full gates exited 0. The real-Docker command used `TOOLU_SERVICE_TEST_IMAGE=toolu-72-gate-tools:local` (Rust 1.94.1); the Linux gate ran the same `./tools/check.sh all` in that image. Base revision: `40840a16bb6bf889bea0c3269a37fb7009b698f4`. The dispatch-only `.github/workflows/service-containers-live.yml` defines identical
pinned-image host/DNS probes for dedicated toolu and official labels. Dispatch
only after both Linux lanes are provisioned; compare exact `SERVICE_74_*_OK`
markers and inspect the printed resource IDs after completion. It has not run.
The captured service acquisition, paired workflow comparison against official runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5` (2.337.0) and GHES remain
**unverified**. The local #75 Docker-action cross-feature replay is recorded below.
No suitable paired online runners were
registered when checked; an offline `toolu-70-final` macOS registration cannot
satisfy the Linux service lane. These missing live lanes are not passing evidence
and do not establish complete epic parity.

The orchestrator confirmed on 2026-09-25 that no GHES endpoint/token exists for
this epic run and authorized delivery with GHES, acquired-service and reference
lanes recorded as unverified. The local gates and real-daemon replay do not close
those missing lanes or establish full parity.

## Workflow and job environment (issue 69)

The production `Runner::execute_job` replay deserializes the sanitized issue-68
acquisition from run [36038637634](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36038637634),
revision `e607a7aae26e113aa3d8ecf1c26c62e0d74192eb`. Its original
`environmentVariables` is empty: `job_environment_layers.json` and the replacement
probe steps are **documented transformations**, not a fresh env-bearing server
capture. The replay keeps incoming context types and differing step UUID/context
names. Bash, Node main/post, composites, and file commands execute for real;
there is no mocked action or successful service response.

| AC / scenario | Observable result | Test / command |
| --- | --- | --- |
| AC-1 / 69-S1 | Workflow `SHARED=workflow`, job `SHARED=job`, later-layer `PRIOR=workflow`, same-layer reference remains workflow; explicit step process and expression both see `step`. All seven incoming roots resolve exactly. | `job_environment_layers_and_step_expressions_reach_real_shell` in `crates/execution/src/execution/tests/job_environment.rs` |
| AC-2 / 69-S2 | Real shell gets empty string, trailing-newline multiline, literal expression-looking text and masked secret probe. Node main/post and nested composite scopes preserve exact empty/multiline/literal bytes. | Above test plus `job_environment_node_and_nested_composite_actions_keep_scopes_and_file_updates` in sibling `job_environment_actions.rs`; existing `secret_sink_coverage_test` covers journal/listener/diagnostic masking paths. |
| AC-3 / 69-S3 | Writer sees job value; next explicit override sees override in process/expression; following step sees file value. | `job_environment_file_updates_and_successive_jobs_are_isolated` |
| AC-3 / 69-S4 | Direct and two nested Node calls see their own action override in main and LIFO posts; file commands persist to outer job while outer SHARED returns to job. Second job sees no first-job SHARED. | Action replay plus successive-jobs test above |
| AC-4 | Absent, empty list, null layer, empty mapping succeed. Scalar bool/number/null coerce to true/42/empty; expression keys work; literal/expression-produced `${{ matrix.tag }}` stays literal. Omitted default wire payloads retain empty/false/zero values. Malformed mapping/value/expression fails setup with no step start; diagnostics omit sensitive expression text. | `job_environment_absent_empty_and_null_layers_are_noops`, `job_environment_scalar_coercion_and_expression_keys`, `job_environment_invalid_layers_fail_before_any_step`, `job_environment_malformed_token_details_do_not_leak_secrets`, `job_environment_upstream_omitted_default_payloads_keep_their_values` |
| AC-5 | Workspace formatting, clippy, suppression ban, guardrails and tests; fixture/action hashes and scenario map are verifiable. | `./tools/check.sh all`; `python3 scripts/test/job_environment_evidence_check.py` |

Focused replay: `cargo test -p execution --lib job_environment` (use the full gate
or approved Docker route in toolu sessions that prohibit the bare command).
All eight focused tests passed in Linux ARM64 Docker using Rust 1.94.1 and real Node 20.
The first three tests were observed failing before the production fix. The host
macOS gate stalled before Cargo in `/usr/bin/env` `_dyld_start`; it is not recorded
as a pass. GitHub `ci` and `ci-macos` validate their respective supported platforms.

`.github/workflows/job-environment-69.yml` runs the same committed probes on hosted
Linux/macOS when this branch is pushed. `workflow_dispatch.inputs.runner` accepts
a JSON runner label or label array for a paired toolu/pinned-reference host. Hosted
runs are **not** proof of the epic's pinned official binary. The workflow tests
service-owned log masking with an actual GitHub token; the token is never written
to the probe output files. Node local `pre` is deliberately absent because the
pinned official runner does not register pre stages for local actions.

`job_environment_evidence.json` records fixture hashes and live-run slots.
`python3 scripts/test/job_environment_evidence_check.py --hosted` verifies a
recorded hosted run's exact action/workflow revision, runner identities, required
successful steps and masked secret markers. `--live` fails closed until both
toolu and pinned-reference evidence are recorded. Fresh env-bearing acquisition,
pinned comparison, live toolu GitHub.com verification, and GHES remain unverified.
Hosted Linux/macOS probes passed in
[run 36197284069](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36197284069)
at `137ac88`. The `--hosted` checker verified exact action/workflow hashes,
successful required steps, runner identities and masked log markers.
Linux/macOS host execution is applicable; Windows is outside the epic. Container
transport is unchanged (#73/#75 own that lane); this change evaluates env before
container declarations so subsequent setup sees the same ordered values.

## Runner version and updates (issue #78)

The protocol compatibility target is **2.337.0**, pinned to official release
`397b032cbf865e9c3ddfab89d533ec19325e1273`. The epic source-inspection snapshot
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5` retains the same version. Toolu's product version is separate
(`0.9.2` in this validation). [Runner updates](runner-updates.md) defines the
operator-managed policy and its eligibility limits.

| Original AC / scenario | Production path and exact result | Runnable evidence |
| --- | --- | --- |
| One version source / 78-S1 | `protocol::build_session_request`, `wire::net::{create_session,poll_message,acknowledge_message}` emit session JSON `agent.version=2.337.0` and poll/ack `runnerVersion=2.337.0`; poll cursor 0/99 and `disableUpdate=true` remain present. The loopback recorder captures real HTTP requests and closes without fabricating successful GitHub responses; all disconnects remain errors. | `cargo test -p wire --test integration -- runner_version::`; test `one_build_emits_one_compatibility_identity_on_all_broker_requests`. Existing `gh_compat_poll_cursor` and `net_test` regressions also pass. |
| Advertised-version rationale / 78-S1, 78-S2 | Protocol constant documents the official baseline; listener startup reports both product and compatibility versions. Same-workflow real GitHub.com canaries compare toolu with the actual official 2.337.0 binary. | `docs/runner-version-evidence.json`; `python3 scripts/test/runner_version_evidence_check.py` checks actual run/job APIs and source checksums. Matching strings alone are not eligibility proof. |
| Refresh decision / 78-S3 | Actual idle and busy handlers ignore RunnerRefresh/AgentRefresh, emit operator guidance, return continue/reset backoff, preserve uncancelled tokens and permit the next job route. Repeated/older IDs are rejected by the production cursor helper. Opaque/empty update bodies bypass decryption and never appear in captured warnings. | `cargo test -p listener --lib runner_update_policy`; `refresh_warns_and_continues_in_idle_and_busy_handlers_without_reading_body`. Input is an explicitly labelled variation of the existing capture-shaped broker envelope; no real refresh delivery is claimed. |
| 30-day policy / 78-S4 | Release maintainer owns baseline/security review and publication; fleet operator deploys/restarts/remints and verifies a canary. Docs describe GitHub's 30-day limit, critical-update exception and queued-job recovery. | `docs/runner-updates.md`, official GitHub policy link, and evidence check's documentation assertions. No local time simulation is offered as service-policy evidence. |
| Real-data coverage / 78-S1–S3 | Actual serialized requests, real toolu/official executables and real GitHub completion results. Refresh replay proves local policy only. | Wire crate sibling integration test, listener sibling tests, committed `.github/workflows/noop-live.yml`, and recorded canary metadata. |
| Required gate / all | Unmodified format, clippy, suppression ban, guardrails and workspace tests. | `./tools/check.sh all`; final result recorded below. |

Both live runners used Linux ARM64 in the same pinned local Rust/gate image and
identical `.github/workflows/noop-live.yml` revision. The workflow has one real
shell step, `echo hello`, with no external actions. This establishes observed
GitHub.com queue/acquire/completion acceptance for that canary at that time; it
is not a guarantee of future eligibility or general workflow parity. The official
2.337.0 release binary reports commit `397b032cbf865e9c3ddfab89d533ec19325e1273`
(the epic's source-inspection commit is separately recorded above). Its Linux
ARM64 archive SHA-256 is
`9b1dc70626422526e3c94767cf024896beb15da5342a3f4819bf2feac13e0393`.

The macOS ARM64 wire and refresh tests passed after intermittent `_dyld_start`
launch stalls; full macOS gate and live macOS comparison remain unverified here.
The authoritative full local gate runs on Linux ARM64. Real GitHub delivery of
refresh controls, a version-gating incident, elapsed 30-day enforcement and GHES
remain unverified. No GHES endpoint/token was supplied; this change makes no new
GHES eligibility claim. Those missing observations are not passing test evidence.

On 2026-09-26, after rebasing onto `c466e41` (PR #136), the unchanged
`./tools/check.sh all` passed on Linux ARM64: **1,012 passed, zero failed,
35 existing ignored tests** (165 test/doctest suites).
The ignored cases are not claimed as live evidence. The worker used Rust 1.94.1
with `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0` after the shared VM
ran out of disk while linking debug symbols; every gate layer remained enabled.
`CARGO_INCREMENTAL=0` bounds cache growth. Log: `/tmp/issue78-gate-last.log`. `python3 -m unittest discover -s scripts/test -p
test_runner_version_evidence_check.py` also checks that empty, omitted, extra and
stale source-checksum maps are rejected against the actual recorded fixture.

Final observed GitHub.com runs: [toolu 36213645647](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36213645647)
and [official 36212900907](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36212900907),
both successful at workflow SHA `fcb7f14ad3f238ebc3c0ed4bac1c1ff3a9324fc5`.
The toolu binary was built from this branch's recorded identity sources on that
base; workflow revision and runner source are distinct. Binary hash, image ID,
runner/job IDs and all seven identity source hashes are committed in
`runner-version-evidence.json`. The final startup log reports product `0.9.2`
and compatibility `2.337.0`; registration, session and acquisition succeeded,
with no acknowledgement-failure warning and a GitHub `success` conclusion.
Both worker-owned ephemeral registrations were cleaned up after the runs.

The post-rebase gate checks integration with #69. The canaries above retain
their actual earlier build base; all seven recorded identity source hashes
remain unchanged after the rebase. They are not a claim that a new binary of
the entire rebased tree was deployed.

## Issue #79 — expression engine parity

The 1,402-case reference corpus is real output from the official SDK at
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`, evaluated with .NET SDK 8.0.425 /
runtime 8.0.31 on macOS ARM64. `crates/expressions/src/tests/` contains the input
expressions, captured typed values/errors and source/input/output hash provenance.
`scripts/expression-reference/generate.sh` reproduces it from that checkout;
`check.sh` validates the committed corpus offline. The oracle context is component
coverage; the production replay below separately proves captured-message wiring.

| Criteria / scenario | Exact evidence and expected observation | Runnable check |
| --- | --- | --- |
| Numeric literals, case arity/types / 79-S1 | `tests::official_runner_corpus_matches_types_values_and_failures`: signed decimal/exponent, radix overflow/two's-complement, NaN/infinities; strict boolean predicates, odd 3–255 arity and lazy selected results match captured typed output/error status. | `cargo test -p expressions` (CI or approved Docker route) |
| Reference equality and NaN / 79-S2 | Exhaustive primitive/container operator pairs in the same corpus. Same context array/object references equal themselves; independently parsed equal containers differ. NaN equality and order false, inequality true. Tiny unequal numbers remain unequal. | Same crate test |
| Case-insensitive ordering, coercion, short circuit, unknown root / 79-S3 | Corpus includes Unicode/ASCII string order and search, primitive-only string builtin coercion, lazy builtin arguments, join separators, null/bool/string/number coercion, absent allowed property → null, unknown root → error even in an unselected branch, unselected `fromJSON('bad')` → no runtime error. | Same crate test |
| JSON/G15/format/filter / 79-S4 | Corpus asserts exact two-space JSON and z-before-a insertion order, nonfinite spellings, signed zero, subnormal/extreme/rounding/exponent boundaries, escaped braces and bad indexes, `.*`/`[*]` projection with absent members omitted. `tests::escaped_format_braces_survive_template_delimiters` asserts `value={a}`. | Same crate test |
| Production fields / 79-S5 | `expression_parity_test::captured_job_replays_expression_fields_through_composite_and_root_script` replays the sanitized #68 `incoming_contexts_matrix_0.json` acquisition through `Runner::execute_job` and real Bash. Documented substitutions retain token types/UUIDs and assign the companion workflow’s authored `parity` action ID (the captured `__self` ID is generated and unavailable in `steps`): top-level if/env/script/with, composite run/env/with/if/shell/outputs, root output `world/world:42/`, missing property empty suffix, escaped braces `{world}`. Negative tests require visible unknown-root failure. | `cargo test -p execution --test integration -- expression_parity_test::` (CI or Docker) |
| Sibling unit tests and full gate / all | Expression unit tests live in `crates/expressions/src/tests/`; production tests in `crates/execution/tests/`. Required gate retains fmt, clippy, no-allow, guardrails and workspace tests. | `./tools/check.sh all` |

The expression engine is shared on Linux/macOS and GitHub.com/GHES. CI runs
platform gates separately. These results do not establish a GHES service/version
claim; no service or UI behavior changes here. The dispatch workflow
`.github/workflows/expression-parity-79.yml` supports equivalent official/toolu
runs at the same workflow/action revision. Live-run links and final gate results
are recorded with the PR; absent live lanes remain unverified. Local Docker passes
all 1,402 reference cases and all three captured-job replays. The host gate passed
its static checks and many test groups, then CLI subprocesses stalled entirely in
macOS `_dyld_start`; GitHub `ci` / `ci-macos` are the explicitly authorized final
gate for this host limitation. The local full gate is not claimed as passing.

Live comparison at `b6d4a67` passed in
[run 36216397520](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36216397520):
`expression-toolu-macos` on `issue79-toolu-macos`,
`expression-pinned-reference-macos` on `issue79-reference-macos` (official
v2.337.0, updates disabled), and `expression-hosted-linux` on Ubuntu 24.04.
Both self-hosted lanes used this same macOS ARM64 host and the identical checkout,
workflow and action revision. Checkout, fixture installation, composite execution,
exact output assertions, and checkout cleanup all succeeded. The pinned ARM64
release archive SHA-256 is
`5a2cd92908a93d7276a194e1de6008099f3e7946f3f8e14aa7a1a7b4a31fdec2`.
Hosted Linux is supplementary execution evidence, not a pinned-reference claim.

## Shell templates (#80)

Contract: actions/runner `cab9d1c3901e45c7705889c4f88284fdd93f4ae5`
(v2.337.0), `ScriptHandler.cs` / `ScriptHandlerHelpers.cs`.

| Criteria / scenarios | Real input and expected observable | Runner |
| --- | --- | --- |
| AC-1, 80-S1/S2 | Real bash/sh isolated PATH: default `false \| true` succeeds, explicit bash fails; no bash/nonexecutable bash selects sh | `cargo test -p execution --test integration -- shell_templates_test::` |
| AC-2, 80-S3 | Perl and custom bash print exact quoted arguments; repeated placeholder and space paths survive; invalid template/unknown shell/missing executable run no marker | same host test |
| AC-3, 80-S4 | Installed python executes `.py` and exits 7; pwsh executes `.ps1`, propagates native exit 7 and stops on Write-Error | `cargo test -p execution --test integration -- shell_templates_test:: --ignored` (requires both interpreters) |
| AC-4, 80-S5 | Captured GitHub.com message replay through Runner proves job defaults, explicit override, composite scope and visible failure | `cargo test -p execution --test integration -- shell_templates_replay_test::` |
| AC-1/AC-4, 80-S1/S5 | Captured Linux container replay proves default sh, explicit bash, translated temp paths and composite custom shell | `bash scripts/test/shell_templates_linux.sh` (real Docker) |
| AC-5 | Complete repository gate, prose and traceable evidence | `./tools/check.sh all`; `python3 scripts/test/shell_templates_evidence_check.py` |

Replay uses `incoming_contexts_matrix_0.json` (GitHub.com run 36038637634,
existing #68 sanitized capture) and `job_container_message.json` (existing #73
capture and adjacent provenance document). Tests retain captured IDs and token
shapes, substituting labelled script/default/shell probe values. Generated local
composite manifests execute real processes; they are not acquired-message claims.
The evidence artifact `crates/execution/tests/shell_templates_evidence.json`
records actual platform results and unresolved lanes. Host semantics apply to
Linux/macOS and GitHub.com/GHES. Job containers are Linux-only. An ignored test,
missing interpreter, unrun live workflow, or absent pinned reference runner is
unverified and cannot establish acceptance parity.

## Composite working directories (#81)

Production replay: `cargo test -p execution --lib composite_working_directory`.
Tests live in `crates/execution/src/execution/tests/composite_working_directory.rs`;
the committed `composite-81-*` action manifests run actual Bash through
`Runner::execute_job`, manifest parsing, expression evaluation and process spawn.
The source acquisition `crates/execution/tests/incoming_contexts_matrix_0.json`
comes from GitHub.com run [36038637634](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36038637634)
(#68); default mapping tokens come from `defaults_run_job.json`, run
[36091513712](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36091513712)
(#71). Transformations retain wire IDs/contextNames and input expression types:
select the local action plus optional final caller step, replace action path and
caller script, and select a dedicated cwd in captured defaults. Setup copies the
exact committed action manifests; it does not construct an ExecutionContext.

| AC / scenario | Exact observable | Test |
| --- | --- | --- |
| AC-1 / 81-S1 | Real `pwd -P` files land in literal, input `world`, `space dir`, prior-output and env directories; absolute `runner.temp` stays absolute; absent, empty and missing-input cwd use workspace. `literal/..` resolves normally. | `literal_expression_absolute_spaces_and_empty_directories` |
| AC-2 / 81-S2 | Repeated child invocations write in `inner` and `second`; child and parent action-path probes remain distinct; root steps ignore caller defaults; the following caller script writes in `caller-default`. | `nested_directories_ignore_caller_defaults_and_restore_scope` |
| AC-3 / 81-S3 | Missing/not-directory paths and malformed expressions prevent the bad script, emit parent-attributed error, return `failure/failure/skipped`, and write exactly `failure\nalways\n` cleanup. | `missing_directory_fails_inner_step_and_runs_cleanup`, `regular_file_directory_fails_inner_step_and_runs_cleanup`, `malformed_directory_expression_fails_inner_step_and_runs_cleanup` |
| AC-3 / 81-S3 continued | Missing cwd retains `failure/success/success`, runs ordinary/always steps, and never runs the invalid-directory script. | `continued_directory_failure_keeps_outcome_and_runs_successors` |
| AC-4 | Format, workspace clippy, no-allow, all guardrails and all workspace tests. | `./tools/check.sh all` |

Expected host semantics match the static upstream contract at
`actions/runner@cab9d1c3901e45c7705889c4f88284fdd93f4ae5` (v2.337.0),
`ScriptHandler.RunAsync`: workspace-relative cwd, no job-default inheritance in
composite scopes. Static comparison is not live parity evidence. Linux/macOS host
behavior applies to both GitHub.com and GHES; no service-owned behavior changes.
GHES acquired-job execution and pinned official-runner comparisons remain
unverified without their external lanes. Container execution reuses the existing
cwd translator; this host replay alone does not prove Docker behavior.

Validation on 2026-09-26: all six focused tests failed before the implementation
and passed afterward on Linux ARM64 with Rust 1.94.1 (0 ignored). The full
`./tools/check.sh all` passed formatting, clippy, no-allow and guardrails, then
failed linking workspace integration tests because the Docker filesystem was
100% full (197 GiB used, 0 available). The full gate is **not passing**.
Native macOS ARM64 compilation was stopped after a build-script sample showed
`_dyld_start` stalled before main. Docker's /Volumes bind mount was unavailable;
the Linux tests used a copied worktree. No stalled, skipped or missing lane counts
as passing. Logs from this worker are `/tmp/issue81-linux-red.log`,
`/tmp/issue81-focused.log` and `/tmp/issue81-gate.log`; these are local evidence,
not durable CI or reference-runner evidence links.

The orchestrator authorized GitHub CI as the full gate after the local dyld/disk
failures. The existing `ci` workflow runs all gate layers on Linux plus clippy and
workspace tests on macOS. These runs, linked from the issue #81 PR, are required
before readiness; local full-gate success is not claimed.

## Docker container actions (#75)

The design contract is `docs/specs/2026-09-25-docker-actions-design.md`.
`execution/tests/docker_action_linux_test.rs` replays the sanitized acquired
job from #73 through `Runner::execute_job`, substituting the committed executable
`.github/actions/docker-action-probe` and documented registry/local step tokens.
These substitutions exercise production dispatch; they are not a new acquisition.

| Scenario | Exact check | Evidence lane |
| --- | --- | --- |
| Docker manifest and raw registry references | `actions::manifest::tests::preserves_docker_stage_and_argument_contract`; `execution/tests/docker_action_test.rs` | Default suite |
| Missing, empty, or whitespace-only registry image rejected before platform/daemon setup | `missing_registry_images_fail_action_resolution_before_platform_or_daemon` | Default suite; also verified without a Docker socket |
| Dockerfile build, immutable cache, registry pull, bad images/builds, timeout and cleanup | `docker::action_container` real-daemon tests | Explicit Linux runtime lane |
| Image-declared anonymous volume removed on cancellation, unrelated named volume preserved | `cleanup_removes_owned_anonymous_volume_but_preserves_unrelated_named_volume` | Explicit Linux runtime lane |
| Exact argv, inputs/default env, mounts, output/env/PATH/state files, workflow commands, LIFO posts | `local_actions_preserve_argv_env_commands_state_and_lifo_posts` | Production Linux replay |
| Absent versus empty manifest args and registry `with.args` | `registry_and_manifest_arg_variants_execute_exact_argv` | Production Linux replay |
| Job network and real peer reachability | `action_joins_job_network_and_reaches_real_peer` | Production Linux replay; service orchestration is #74 |
| Service-only host job: main/output and post reach nginx by alias; service/container network removed afterward | `docker_action_joins_service_only_network_through_main_and_post` | Production Linux replay across #74/#75 |
| Invalid entrypoint, failed-main post, observed-start cancellation, owned-resource cleanup and unrelated-container survival | `entrypoint_failure_and_cancellation_fail_visibly_without_leaking_containers` | Production Linux replay |
| Remote downloader, image reuse, pre/main/post state and cleanup after failed pre | `remote_repository_action_runs_pre_main_post_and_reuses_image` | Explicit post-push remote replay |
| Non-Linux rejection | `execution/tests/docker_action_test.rs` platform-specific test | Default macOS suite |

Run `bash scripts/test/docker_actions_linux.sh all` with a local Linux Docker
daemon whose bind mounts can access `TOOLU_DOCKER_ACTIONS_LINUX_ROOT`. The carrier
image defaults to Rust 1.94.1 and can be supplied through
`TOOLU_DOCKER_ACTIONS_TEST_IMAGE`; runtime/replay use real containers and must be
explicitly selected because ordinary workspace tests ignore the Linux probes.
The unchanged full repository gate remains `./tools/check.sh all`.

The real-daemon runtime tests and five production replay tests passed on
Linux ARM64 against the local daemon on 2026-09-25. After rebasing onto #74,
the service-only test reproduced the missing-network failure with a corrected
port-80 probe, then passed for both main and post with the typed network fallback.
The Ubuntu `ci` job explicitly
runs both ignored lanes after the default workspace tests; an ordinary ignored
result is not counted. The initial implementation passed the unchanged full gate
in the Linux carrier; the final rebased diff reruns that gate before delivery and
is checked by `ci`/`ci-macos` on the PR.
The host gate passed formatting, Clippy and guardrails, then was stopped after
`sample` confirmed `context_secret_batch_test` stalled in `_dyld_start` before
the harness. This is an incomplete host run, not a test pass.
The branch-scoped `docker-actions-live.yml` workflow
is an executable live probe; its presence does not establish a live pass. Paired
toolu/reference execution, a newly acquired Docker-action payload, and GHES
remain unverified until their run identities and observations are recorded.

## Action metadata (issue #85)

The replay starts from `crates/execution/tests/incoming_contexts_matrix_0.json`,
a sanitized GitHub.com #68 acquisition. It preserves wire UUID/name/contextName
and template-token structure, removes remote checkout, substitutes committed
`.github/actions/action-metadata-probe` actions and shell probes, and runs
`Runner::execute_job`. Named/absent-name variants are explicit mutations of that
capture. Probes use actual Bash/Node processes and files, not mocked responses.

| AC / scenario | Check and exact result | Lane |
| --- | --- | --- |
| AC-1 / 85-S1 | `action_metadata_captured_names_nested_stages_and_restoration`: `__run`, `__run_2`, `__self`, `__self_2`, `__run_3` in exact process-file order; empty repo/ref on run/local steps. | Linux/macOS workspace tests |
| AC-1 / 85-S1 | `action_metadata_wire_name_named_skipped_and_legacy_absence`: wire `explicit_name` wins over `output_scope`; skipped step never executes; absent name/context exports empty, never UUID; `action_metadata_empty_wire_name_uses_legacy_context_name` verifies legacy fallback. | Linux/macOS workspace tests |
| AC-1–2 / 85-S1–S2 | `action_metadata_remote_dispatch_repeated_refs_and_parent_restore`: captured remote `actions/checkout` reference through production resolved-action dispatch with the real committed probe; exact v4 and topic/branch refs, subpath excluded, child with-input repo empty, remote composite parent output restored after local children and after a missing pre script. | Linux/macOS component integration; **does not verify live download resolution** |
| AC-2 / 85-S2 | Captured replay records distinct pre/main/post markers, two nested child calls per composite and two composite invocations, including continued missing-action failure and always cleanup. Posts inherit their originating top-level name in LIFO order. | Linux/macOS workspace tests |
| AC-1–2 / 85-S1–S2 | `action_metadata_absent_identity_survives_composite_posts`: removing both wire name and contextName from a captured composite keeps the action identity empty in both child invocations and deferred posts; exact pre/main/child/parent/post marker order. | Linux/macOS workspace tests |
| AC-3 / 85-S3 | Each shell probe compares github metadata and runner.environment expressions with actual process variables. Every stage records self-hosted. | Linux/macOS workspace tests |
| AC-1,3 / 85-S1,S3 | `action_metadata_host_job_container_and_docker_actions`: real pinned Alpine registry/local Docker actions, plus shell steps before/after, with and without the captured Ubuntu job container; exact names, empty repo/ref and self-hosted. | Linux Docker, explicitly invoked by CI |
| AC-4 | `./tools/check.sh all`; GitHub.com `ci` and `ci-macos` workspace tests, plus the existing Linux Docker replay step. | Required gate and CI |

Host checks live in `crates/execution/src/execution/tests/action_metadata.rs`:
`cargo test -p execution action_metadata`. Docker checks live in sibling
`action_metadata_docker.rs`, included by `docker_action_linux_test.rs`:
`TOOLU_CONTAINER_TEST_ROOT=/tmp/toolu-docker-action-tests cargo test -p execution
--test integration -- docker_action_linux_test::action_metadata --ignored --test-threads=1`.
The test root must be shared with the Docker daemon; absent tools/access fail.
Node probes may download the pinned Node 20 runtime when not cached.

Local and CI results are recorded in the issue PR. Per the orchestrator's
2026-09-26 decision, GHES, new live acquisitions and equivalent pinned official
runner comparisons are **unverified/skipped**, not inferred from replay and not
blocking this delivery. macOS Docker is not applicable (unsupported platform).
The pinned upstream contract inspected is actions/runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`; the tests do not claim reference parity.

## Issue #84 — problem matchers

Run `cargo test -p execution problem_matcher` and `./tools/check.sh all`.
The sibling tests under `crates/execution/src/execution/tests/` use setup-node
49933ea5288caeca8642d1e84afbd3f7d6820020 definitions with captured TypeScript 5.9.3
and ESLint 8.57.1 output. `problem_matcher_provenance.md` records exact capture
commands and normalization. No mocked service success is used.

| Criterion/scenario | Input and production evidence | Expected result |
| --- | --- | --- |
| JSON loading, annotation matching; 84-S1 | `problem_matcher` component tests and `problem_matcher_real_tsc_stderr_and_nested_scope`, `problem_matcher_real_eslint_loop_and_remove` | TSC error at src/example.ts:1:7; ESLint warning at src/example.js:1:5 and error at 2:13, assigned to actual report UUID. |
| Multiline, paths, ANSI; 84-S2 | Captured ESLint loop; `problem_matcher_from_path_ignores_process_cwd`, `problem_matcher_partial_loop_does_not_cross_steps`; colored captured text component test | Loops retain filename, new process state resets, fromPath uses its parent and workspace despite changed cwd. |
| Lifecycle/invalid/pathological; 84-S3 | Registry tests mutate pinned definitions; `problem_matcher_bad_json_fails_command_step`; bounded long-line component test | Atomic owner replacement, case-insensitive removal, invalid JSON fails step, long lines do not advance state. |
| Masking/nested actions; 84-S4 | `problem_matcher_masks_real_diagnostic_and_file_removal`, nested TSC replay | Masked diagnostic and parent report UUID; registration persists into nested action. |
| Gate/platform applicability | Full repository gate and existing github.com `ci` / `ci-macos` jobs | Linux/macOS production replay and workspace regressions; CI link supplied in PR when verified. |

Replay imports the existing sanitized `incoming_contexts_matrix_0.json` acquired
message, preserving UUID/contextName and map token structure while replacing
selected probe scripts. The nested-action replay also substitutes a local action
path. Bash executes the real captured output files; these tests cover production
execution/event wiring, not GitHub-owned rendering. Live GitHub.com/GHES service/UI and the official-runner comparison
lanes are **unverified/skipped** under the orchestrator's delivery scope. No GHES
version, Docker runtime, or universal .NET regex parity is claimed by these tests.

## Set up job diagnostics (#87)

The listener replay uses the sanitized GitHub.com acquisition
`crates/execution/tests/incoming_contexts_matrix_0.json` (provenance in
`incoming_contexts_evidence.json`). It preserves acquired metadata and wire
structure, replaces workflow steps with a real shell probe, removes remote
checkout and live endpoint routing, and introduces a real filesystem obstruction
or an actual cancelled token for failure cases. Metadata absence and malformed
permissions are explicit boundary transformations of that capture.

| AC / scenario | Test and observable result | Applicable lane |
| --- | --- | --- |
| Original identity/permissions/source criteria; AC-1 / 87-S1 | `setup_capture_keeps_metadata_without_results_endpoint`: `Default`, `Contents: read`, `Metadata: read`, `Secret source: Actions`, permission group despite no results URL. `setup_capture_has_actual_host_identity_and_explicit_absence`: actual version/OS/arch/hostname; missing group/name unavailable, absent optional blocks omitted. | Linux/macOS workspace |
| AC-1 / 87-S1 | `setup_capture_malformed_permissions_are_not_reflected`: fixed parse diagnostic, no reflected malformed input or unclosed group. | Linux/macOS workspace |
| Original action SHA criterion; AC-2 / 87-S2 | `setup_action_real_archive_cold_warm_dedup_and_subpath`: real captured REST revision response and unmodified archive through `ActionFetcher::for_job` / `ensure_action`; one archive request across duplicate/subpath and new-job disk cache; exact ref/SHA lines, no service URL. | Linux/macOS recorded public-service replay |
| AC-2 / 87-S2 | `setup_deferred_action_log_retains_upload_and_live_identity`: post-completion diagnostics retain setup UUID in live/upload channels and combined log; serialized completion preserves setup name, number and start time. | Linux/macOS component test with captured revision |
| AC-2 / 87-S2 | `setup_action_production_prefetch_emits_resolved_sha`: captured message → `Runner::execute_job` → production prefetch; real shell observes completed extraction; exact immutable SHA log emitted. Remote execution is condition-skipped, so this is not Docker runtime evidence. | Linux/macOS workspace |
| AC-2 / 87-S2 | `setup_action_failed_download_has_no_success_identity`: closed real socket causes failure, no invented successful download line. | Linux/macOS workspace |
| AC-1,3 / 87-S3 | `setup_production_replay_streams_metadata_and_completes_before_shell`: actual listener startup, Runner, collector and forwarder; captured secret in group name becomes `***` in live output; setup completion precedes real shell start. | Linux/macOS workspace |
| AC-3 / 87-S3 | `setup_production_failure_preserves_partial_masked_live_log`, `setup_production_cancellation_preserves_metadata`, `setup_failed_real_hook_keeps_named_preparation_output`: real failing shell hook/filesystem failure/pre-cancel retains source/permissions and masked output; setup reports Failure/Cancelled respectively. | Linux/macOS workspace |
| Original real-data/gate criteria; AC-4 | `./tools/check.sh all`; GitHub.com `ci` and `ci-macos`. | Full gate and CI; PR records results |

Listener tests are in sibling `crates/listener/src/tests/setup_step.rs` and
`setup_forwarding.rs`; action tests are in
`crates/execution/src/execution/actions/tests/setup_actions.rs`. The public action
is `actions/hello-world-docker-action` at
`8bcd8e1af3c095561f1043123848fc8b2db0f189`, captured 2026-09-27. The committed
`setup_action_commit.json` is the real REST response; `setup_action.tar.gz` contains
unmodified codeload bytes and upstream LICENSE. `setup_action_revision.json`
records URL and archive digest. Local static endpoints replay those captures;
only job API routing is redirected. No successful resolver response is invented.

Private/scoped download acquisition, GHES, live GitHub UI/service delivery and an
equivalent pinned official-runner comparison are **unverified/skipped**, per the
orchestrator policy. Public recorded-response replay cannot prove those lanes.
The live-output assertions inspect the actual outgoing channel, not a GitHub UI.
The upstream contract inspected is actions/runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5` JobExtension.cs. Current self-hosted OS
and architecture come from the host, not hosted-runner image metadata.

## Step summaries (#83)

- `cargo test -p execution --lib step_summary`: captured acquisition replay with
  real shell and committed Node/composite actions; append/overwrite, no content,
  deletion with preserved outputs, UTF-8 byte limit, registered/dynamic masking,
  later failure and cancellation, distinct document IDs (83-S1–S4); a BOM-only
  file contributes nothing, a symlink at the summary path is rejected without
  uploading the file it points at, and a FIFO there is rejected without blocking.
- `cargo test -p listener step_summary`: failed HTTP uploads still drain later
  queued documents. This is failure-path coverage, not successful-service proof.
- `cargo test -p wire step_summary`: local TCP stubs cover a failed RPC, a
  malformed reply that never exposes values, and blob 503 retries without
  leaking the signed URL. The successful three-call upload is not exercised.
- `cargo test -p observability step_summary`: journal includes only summary ID
  and byte count, never document text.
- `.github/workflows/step-summary-83.yml`: opt-in GitHub.com live toolu/reference
  Linux/macOS and Linux-container probes. Live evidence pending; GHES explicitly
  unverified/skipped. See [step summaries](step-summaries.md).

## Step attributes and display names (#99)

`continue-on-error`, `timeout-minutes` and the display name are template tokens
evaluated at execution time, like upstream's `StepsRunner` /
`ActionRunner.GenerateDisplayName` at `cab9d1c3901e45c7705889c4f88284fdd93f4ae5`.
The replays use the toolu-lane job messages acquired in run 37992025403
(`crates/execution/tests/step_attrs_99_*.json`, sanitized to UUIDv5 placeholders)
with real Bash and Node and the committed `step-attrs-99-*` actions. The
[workflow](../.github/workflows/step-attrs-99.yml) runs each job on a toolu lane
and a GitHub-hosted reference lane. Run
`python3 scripts/test/step_attrs_evidence_check.py --local` to validate the
mapping and hashes in `crates/execution/tests/step_attrs_evidence.json`. Without
`--local`, it fails while a required lane is unverified.

| AC / scenario | Exact observation | Tests | Live (run 38007470498) |
| --- | --- | --- | --- |
| AC-1, AC-5 / S1 | Absent, literal, `steps.*` and `matrix` attributes keep their wire types. Continue-on-error gives failure/success. Literal `false`/`0` and a negative timeout leave the step unbounded and green. | `template_token_test`, `captured_literal_deferred_and_matrix_attributes` | Both lanes pass every check step. |
| AC-2, AC-5 / S2 | `fromJSON(steps.prior.outputs.minutes)` kills `sleep 600` after one minute with `The action 'Deferred timeout' has timed out after 1 minutes.` Pre and post stages apply continue-on-error separately. | `captured_deferred_timeout_bounds_a_real_process`, `deferred_continue_on_error_reads_prior_step_output`, `failing_pre_honors_continue_on_error`, `failing_post_honors_continue_on_error` | Identical timeout line on both lanes. |
| AC-2 / S3 | `1.9` truncates to one minute. A string timeout logs the reference's two error lines and applies no bound. A string continue-on-error logs its two lines and keeps the failure, including in composite `action.yml`. | `step_attrs` unit tests, `captured_fractional_timeout_truncates_to_whole_minutes`, `captured_string_timeout_logs_error_and_applies_no_bound`, `captured_string_continue_on_error_keeps_the_failure`, `composite_continue_on_error_expressions` | Identical lines on both lanes. `invalid-*` fails on both and `composite-coe-*` passes on both. |
| AC-3 / S4 | Job-start names come from message contexts. Before the condition, unevaluated names are retried with live contexts. A failure warns in "Set up job" and then at the step, and keeps the earlier name (`run` if job start already failed). Pre stages use the job-start name and post stages the final name. | `step_display` unit tests, `display` tests, `captured_display_names_match_the_reference`, `pre_timeout_reports_the_pre_step` | Every jobs-API step name and conclusion matches the reference. Warnings appear at the same places. |
| AC-4 / S5 | Composite children share one budget: the second `sleep 40` gets `The action has timed out.` Node pre and main each get the full budget. | `captured_composite_children_share_one_budget`, `pre_and_main_get_separate_timeout_budgets`, `step_timeout` unit tests | Check steps pass on both lanes. The nested timeout line is identical. |

Known differences from the reference:

- `fromJSON` parse errors inside display-name warnings carry serde_json's message instead of Newtonsoft's.
- Composite `action.yml` attribute errors omit the reference's `<action.yml> (Line, Col)` prefix.

Step numbering and the exit-code trailer also differ (the pre step is reported as step 0, and post rows are numbered sequentially rather than with upstream record orders). #88 added the trailing "Complete job" row; pre-stage ordering stays with the pre/post work.

Platform applicability: the GitHub.com Linux lane passed. The toolu lane ran the
stack-head debug build on Linux x86_64; the reference lane was `ubuntu-24.04`
runner 2.337.0, which is newer than the source pin. macOS live is **unverified**:
evaluation is platform-independent, but real-process timeouts ran only on Linux.
The gate's tests run on macOS CI. GHES is **unverified** because no server is
configured; it uses the same job-message tokens. Docker pre stages share the
naming path; the Docker-specific timeout lines were verified with the ignored
real-Docker tests in `docker::job_container_failures`.

## Issue #89 — orphan process cleanup

`RUNNER_TRACKING_ID` follows upstream `JobExtension.cs:586-600` / `880-925` at
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`; see the
[design](specs/2026-10-10-orphan-process-cleanup-design.md). The replays use the
sanitized #68 GitHub.com acquisition (`crates/execution/tests/incoming_contexts_matrix_0.json`,
`__run` / `__self` steps) through the production `Runner` → `run_job` →
`finish_execution` path, with real Bash, Node (`orphan_89_*` fixture action),
python3 `os.setsid` and `ps -o stat=`. No mocks. Red evidence: the same test
file at the pre-implementation commit failed 11 of 13 (missing id `unset`,
missing cleanup header); the two negative cases passed trivially.

| AC / scenario | Exact observation | Tests (`cargo test -p execution …`) | Platforms |
| --- | --- | --- | --- |
| AC-1 / issue AC 1 | Hook, `run:` step, Node main and Node post print one identical `github_<uuid v4>`; a second job prints a different one. A re-exec with `RUNNER_TRACKING_ID=github_outer89` still prints the job id, and `github_outer89` when `process.clean=false`. | `--test integration -- orphan_cleanup_test::`: `tracking_id_reaches_hook_shell_and_node_stages`, `tracking_id_replaces_inherited_outer_value` | Linux x86_64 local (root); CI `ubuntu-latest` and `macos-14` via `cargo test --workspace` |
| AC-2 / 89-S1 | A stdout-holding child, a `bash -c` grandchild, `nohup` and `setsid` sleepers are gone or zombies after success, `exit 1` and cancellation; each swept one has `Terminate orphan process: pid (N)`; the job ends well inside 60 s (the stdout holder costs the 2 s drain). A job with no orphans logs only the header. On cancel the step-group `killpg` already reaps the non-detached kinds, so only the `setsid` one reaches the sweep. | `orphans_are_killed_after_successful_job`, `…_failed_job`, `…_cancelled_job`, `job_without_orphans_logs_header_and_no_termination` | as AC-1 |
| AC-3 / 89-S2 | Two concurrent jobs: A's sweep leaves B's detached process and an unrelated id-less `sleep` alive (non-zombie `ps` state); B's own sweep then kills B's. | `concurrent_job_and_unrelated_process_survive_cleanup` | as AC-1 |
| AC-4 / 89-S3 | Step `env: RUNNER_TRACKING_ID: ''` prints empty and its process survives; an upper-cased re-export is still killed; `process.clean=false` logs no header, kills nothing and passes the inherited value (or `unset`). Parser: `False`, ` FALSE ` disable; `0`, `no`, garbage enable. | `empty_step_tracking_id_opts_out`, `upper_cased_tracking_id_still_matches`, `process_clean_false_disables_tracking_and_cleanup`; `--lib orphan_cleanup` | as AC-1 |
| AC-5 / 89-S2 lookup failure | A tagged process that unsets the id and `exec`s `sleep` between scan and kill keeps its pid but loses the id: not signalled, still running. An unreaped zombie carrying the id is skipped without error. An expired deadline reports `timed_out` and kills nothing. A live tagged `sleep` dies by signal 9. | `--lib orphan_cleanup`: `exec_without_id_between_scan_and_kill_is_not_signalled`, `unreaped_zombie_carrying_id_is_skipped`, `expired_deadline_kills_nothing_and_reports_timeout`, `sweep_kills_a_live_tagged_process` | as AC-1 |
| AC-6 / 89-S4 | A job-started hook that detaches and exits 1 (job `Failure`) and a Node post that spawns `detached: true` and exits 1 (job `Failure`) both have their process killed. Cancel + shutdown fired on the cleanup header still kill every orphan and close the stream. | `failed_job_started_hook_orphan_is_killed`, `failing_node_post_orphan_is_killed`, `cancellation_during_cleanup_still_kills` | as AC-1 |
| AC-7 / Non-Goal 1 | Captured #73 job-container message with real Docker 29.1.3: the container step prints `TRACK89\|container\|unset`, the host sweep logs its header and terminates nothing. | `TOOLU_CONTAINER_TEST_ROOT=<dir> cargo test -p execution --test integration -- orphan_cleanup_container_test:: --ignored` | Linux + Docker, run manually (ignored by default, so **unverified in the gate**); macOS not applicable |
| AC-8 / gate + docs | `./tools/check.sh all` exits 0; README "What runs", `docs/architecture.md`, `AGENTS.md` and module READMEs describe the behavior and limits. | gate | Linux local, CI |

Reference parity: [`orphan-cleanup-89.yml`](../.github/workflows/orphan-cleanup-89.yml)
runs [`orphan_cleanup_probe.sh`](../scripts/test/orphan_cleanup_probe.sh) — the
same orphan kinds plus an opted-out process — on GitHub-hosted `ubuntu-24.04`
and `macos-15` (official runner); its "Complete job" log must show `Terminate
orphan process` for each `ORPHAN89|tagged` pid and none for the
`ORPHAN89|opted-out` pid.

Observed in [run 38101935911](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38101935911)
(head `200ba23`, official GitHub-hosted runner). On both `ubuntu-24.04` and
`macos-15` the step saw a `github_<uuid>` id. "Complete job" logged `Cleaning
up orphan processes`, then one `Terminate orphan process: pid (N) (name)` for
each of the four tagged pids (`sleep` ×3 plus `python3`/`Python`), and none for
the opted-out pid. Those are the same lines and the same kill/spare split that
toolu's replay tests assert.

Limits stated, not claimed: the environment match finds only processes
started with the variable and readable by the runner's user (Linux
`/proc/<pid>/environ`, macOS `KERN_PROCARGS2`, both exec-time snapshots). With
SIP on, macOS returns no environment for Apple platform binaries, so a second
match also sweeps same-user processes started during the job inside a host
step's registered process group (`orphan_cleanup::tests::group_*`,
`a_group_whose_id_now_names_another_leader_is_spared`,
`group_matches_need_a_start_since_the_job_and_the_runners_uid`,
`release_forgets_only_groups_left_empty`,
`member_execing_away_the_id_is_retried_by_its_group`, and the replay tests
`id_less_member_of_a_{run_step,node_post,composite_step,job_hook}_group_is_killed`).
A `sudo` process, another user's process, or one that left the step's group
(`setsid`/`setpgid`) while hiding or dropping the id still escapes. The replay
tests need a `python3` whose environment the kernel shows (`Replay::new`
precondition). They and the env-path unit tests (which switch to ad-hoc
re-signed `sleep` / `bash` copies only where SIP hides the environment) were
run locally on a SIP-enabled macOS 27.0.1 host. macOS real-process behavior is verified by
the CI `macos-14` test run and, for SIP-hidden environments, by that local
macOS 27.0.1 run; a macOS toolu live lane is **unverified**.
The toolu self-hosted workflow lane (`run_toolu`) is **unverified** (no
registered runner). GHES is **unverified** (no server); the process rule does
not depend on the server.

# CompleteJob payload parity (#88)

`completejob` now matches the official runner's `CompleteJobRequest` /
`StepResult` contract at actions/runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`. It carries the evaluated
`environmentUrl` ("View deployment"), the echoed `billingOwnerId`,
`infrastructureFailureCategory` for runner-side action fetch failures, and
`StepResult`s with upstream `snake_case` keys, string `status`/`conclusion`,
`action_name`/`ref`/`type` and per-step annotations. A trailing **Complete
job** row evaluates job outputs and then the URL, as upstream's
`JobExtension.FinalizeJob` does.

Captures: the toolu-lane acquisitions of
[`completejob-88.yml`](../.github/workflows/completejob-88.yml) run
[38098611227](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38098611227),
sanitized with UUIDv5 placeholders (`crates/execution/tests/completejob_88_*_message.json`;
hashes in `crates/listener/tests/completejob_88_evidence.json`). Check them with
`python3 scripts/test/completejob_evidence_check.py --local`. Boundary cases are
labelled capture-derived: they rewrite only the URL token or one script. Action
resolver and archive failures use local fault-injection servers whose injected
statuses are the faults under test.

| AC / scenario | Exact observation | Tests |
| --- | --- | --- |
| AC-1 / 88-S1 | `steps.deploy.outputs.url` gives `environmentUrl` `https://toolu-88.example/toolu/38098611227` and logs `Evaluated environment url: …` in "Complete job". Absent or null → no key. Literal, `""`, missing output, `not a url`, `1`, `true` and `null` are sent as upstream strings. The final `$GITHUB_ENV` value is used. A mapping, a sequence, a bad `fromJSON` or `secrets.*` fails "Complete job" and the job. | `environment_url_test` (execution); `completejob_env_capture_sends_url_billing_and_step_identity` (listener) |
| AC-2 / 88-S2 | A runtime `::add-mask::` value, a message mask hint, or a literal URL containing a secret is dropped with the warning annotation `Skip setting environment url as environment '<name>' may contain secret.` (absent name → `''`). The token never appears in the POST, and every journal/log line masks it. | `captured_runtime_masked_url_is_suppressed_with_a_warning`, `mask_hint_and_literal_secret_urls_are_suppressed`, `secret_environment_url_never_reaches_completejob_or_the_masker_sinks`, `mask_hint_url_is_suppressed` |
| AC-3 / 88-S3 | Every posted `StepResult` uses only upstream keys. `external_id` is the wire UUID. `run`+`bash`/`sh`, `node20` + local path (main and post), `composite` + path, `Dockerfile` + path, `DockerHub` + `alpine:3.20`, and `runner` + `setup_job`/`complete_job`. A remote `owner/repo[/path]` + ref is covered by unit tests. A failing `::error::` step under continue-on-error keeps its annotation. Composite children are never rows. Pre and post rows carry their action's label; a post skipped by `post-if` carries none (upstream labels only a row whose handler ran). | `step_metadata_test` (Docker rows `--ignored`, also in CI's Docker step), `step_metadata` unit tests, `step_metadata_reaches_every_posted_step_result`, `pre_main_and_post_rows_carry_action_metadata_and_a_skipped_post_none`, `completejob_wire_test` |
| AC-4 / 88-S4 | `billingOwnerId` is echoed (`U_kgDOAH-IyQ`) and omitted when absent. Launch 500/429 → `resolve_action`. Archive 500/404/401×2/mid-body cut → `error_download_action`. Corrupt gzip → `invalid_action_download`. Each carries an `isInfrastructureIssue` FAILURE annotation on the failing (or enclosing composite) step. Launch 422, archive 403, 401-then-success, a failing script and the outage watchdog set no category. A continue-on-error fetch failure keeps its annotation but sets no category. The first failed step's category wins; a cancelled step never latches one. The infrastructure message is masked before the POST and the journal. A body that fails to parse still echoes `billingOwnerId`. | `action_infrastructure_test`, `infrastructure_category_is_sent_only_for_a_failed_runner_side_fetch`, `first_failed_step_category_wins_and_only_failures_latch`, `infrastructure_error_message_is_masked_in_completejob_and_the_journal`, `unparseable_job_still_echoes_the_billing_owner`, `completejob_without_billing_owner_omits_it`, `action_fetch_categories_follow_the_upstream_exception_split` |
| AC-5 / 88-S5 | A 503 then 200 `completejob` re-sends a byte-identical body through `retry_transient`. A job cancelled mid-step still runs "Complete job" and posts `canceled` with the URL. | `completejob_retry_resends_an_identical_body`, `completejob_cancelled_job_keeps_the_url`, `cancelled_job_still_reports_the_url_from_completed_steps` |
| AC-6 / live | See the live table below. | `python3 scripts/test/completejob_evidence_check.py` |
| AC-7 | Repository gate and this documentation. | `./tools/check.sh all`; CI's "CompleteJob evidence (#88)" step runs `completejob_evidence_check.py --local` and `--docs` |
| AC-8 | The journal carries one `step_metadata` line per reported row (including "Complete job") and an `error` annotation line for an infrastructure failure. The canonical fixture is regenerated from a real engine run. | `journal_writer_test`, `journal_reader_test`, `journal_types_test` |

Live comparison (AC-6): [run 38104866197](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38104866197),
toolu built from `5d727e4` on Linux x86_64 (`toolu-88-verify`) against the
GitHub-hosted `ubuntu-24.04` official runner on the same workflow SHA. API
results are recorded in `crates/listener/tests/completejob_88_evidence.json`;
`python3 scripts/test/completejob_evidence_check.py` checks them and compares
the toolu lane's rows and annotations with the reference lane's, allowing only
the reference-only differences listed below. `--strict` also fails while macOS
and GHES are unverified.

| Check | toolu | reference |
| --- | --- | --- |
| `url` job deployment `environment_url` | `https://toolu-88.example/toolu/38104866197` | `https://toolu-88.example/reference/38104866197` |
| `secret` job deployment `environment_url` | absent | absent |
| `secret` job check-run annotation | `Skip setting environment url as environment 'completejob-88-secret-toolu' may contain secret.` | same text for `…-reference` |
| Last row of every job | "Complete job" | "Complete job" |
| Job logs load (lines: url / steps / secret) | 22 / 111 / 20 | 42 / 199 / 39 |
| `steps` job annotations | `step failed on purpose`, `composite child notice` | same, plus Node 20 deprecation and exit-code lines |

GitHub accepted the upstream-shaped `StepResult`s (snake_case keys, string
enums) on every toolu job. The capture run 38098611227, which already used
the new shape, completed the same way. macOS and GHES are **unverified**: no
macOS runner host or GHES server is available here (recorded in the evidence
file). GitHub does not expose `action_name`/`ref`/`type`, so those fields are
verified at the outgoing POST only.

Known differences from the reference:

- toolu attributes action fetch failures to the failing step. Upstream fetches
  during "Set up job", so its annotation sits there.
- A literal environment URL containing a registered secret is dropped. Upstream
  would send it.
- toolu's evaluator words an unknown context `unknown named value: secrets`
  where upstream says `Unrecognized named-value: 'secrets'`.
- `type` for a Node action is the runtime toolu runs (`node20` for a `node20`
  manifest). Upstream may report `node24` when GitHub forces Node 24.
- "Complete job" log content differs: toolu's row ends with a
  `Job conclusion: <conclusion>` line so it always has a log. A failed
  job-started hook, or an internal runner error that aborts the step loop,
  still ends the job without a "Complete job" row.
- The reference lane has a `Pull alpine:3.20` row (upstream pre-pulls registry
  images) and the Node 20 deprecation and `Process completed with exit code`
  annotations; toolu emits neither.
- `infrastructureFailureCategory` `debugger_tunnel_failure` is never produced:
  toolu has no step debugger.
- GHES: toolu completes GHES jobs through the same Run Service call. The V1
  `JobEvent` completion is not implemented.


## Session-create conflicts (#91)

**Live capture.** On 2026-10-11 two throwaway github.com JIT registrations
(runner ids 158 and 159 on this repo, label `toolu-91-probe-only`, deleted
afterwards) ran the production `wire::net::authenticate` and
`protocol::build_session_request`. A second `POST {server_url_v2}/session`
while the first session was live returned **409, `content-length: 0`, empty
body**. The verbatim reply (headers kept, no credential present) is
`crates/wire/tests/session_conflict_409.json`, with a `provenance` block. After
deleting the first session, `create_session` kept returning 409 for ~181 s,
then succeeded. Upstream's 30 s × 8 window (pinned `cab9d1c`,
`MessageListener.cs` / `Constants.cs` / `BrokerHttpClient.cs`) covers that.

| Criterion / scenario | Real input and production path | Expected result | Runnable check |
| --- | --- | --- | --- |
| 409 → dedicated error | Captured reply replayed as raw HTTP to `create_session` | `RunnerError::SessionConflict`, Display "another runner session is active for this registration"; token not echoed | `cargo test -p wire --test integration -- session_conflict::` (`captured_409_is_a_session_conflict`) |
| Fixture is the capture | Committed fixture | status 409, empty body, `content-length: 0`, provenance recorded | same suite, `fixture_is_the_verbatim_live_capture` |
| 91-S1 recover within budget | Capture then a session, through `retry_session_conflict` + `create_session`; and through the real `boot` binary to a completed job | 2 session POSTs, session created; `boot` exits 0 | `cargo test -p listener --lib session_conflict` (`conflict_then_success_creates_the_session`); `cargo test -p toolu-runner --test integration -- session_conflict_e2e_test::` (`boot_recovers_when_the_conflict_clears_inside_the_window`) |
| 91-S2 persistent conflict | Capture on every POST | exactly 8 attempts, then `SessionConflict`; `boot` exits **5** | `persistent_conflict_stops_after_max_attempts`; `boot_exits_5_on_a_persistent_conflict` |
| 91-S2 unrelated failures | 401 / 403 / 500 directly, and 401 / 500 after a conflict | stay `Protocol`; retry stops after 2 POSTs | `other_failures_keep_the_protocol_classification`; `unrelated_failure_after_a_conflict_keeps_its_classification` |
| 91-S2 cancellation | Cancel / SIGTERM during the default 30 s wait | `Ok(None)` in < 2 s; `boot` exits 0 in < 5 s after SIGTERM, 1 POST | `cancel_during_the_retry_wait_returns_none_promptly`; `sigterm_during_the_conflict_wait_exits_zero_promptly` |
| 91-S3 default loop / `--once` | Persisted registration whose JIT points at the replaying broker; no login token | `run` and `run --once` exit **5** after exactly 8 POSTs. A re-mint would exit 2 without a token, so this also proves no generic backoff or re-mint | `default_run_loop_exits_5_without_backoff_or_remint`; `run_once_exits_5_on_a_persistent_conflict`; decision table `cargo test -p listener --test integration -- loop_decision_test::` |
| 91-S3 logs | Same runs | stderr has `session conflict on attempt 1/8` and the conflict message; the access token and JIT blob are absent from stderr and every `_diag` file | `assert_conflict_exit` in the e2e suite |
| 91-S3 service restart | Rendered systemd unit + legacy `scripts/toolu-runner.service` | `RestartPreventExitStatus=5` in both | `cargo test -p config --test integration -- service_unit_test::` |
| Gate | Whole workspace | green, unmodified | `./tools/check.sh all` |

Red evidence: before the fix the same e2e cases at `origin/main` exited **1**
(`boot`) and **2** (`run --once`), and the wire test saw `Protocol`.

**Unverified / not applicable.** A live persistent-conflict run of the full
binary against github.com was not repeated: the live probe exercised the
transport and recovery window only. No official-runner live comparison was
run; parity is with upstream source. GHES: the same `create_session`
transport serves it, but no GHES server is available, so this is
**unverified**. launchd cannot condition `KeepAlive` on an exit code, so on
macOS a supervised runner relaunches into a fresh bounded window; this is a
documented limitation. The tests are platform-neutral (Linux run here); macOS
is **unverified**.


## Registration CLI parity (#92)

`register --no-default-labels` / `--ephemeral`, parity with upstream
`config.sh` (pinned `cab9d1c`: `CommandSettings.cs`, `Validators.cs`,
`ConfigurationManager.cs`, `Runner.cs`, `MessageListener.cs`). Spec:
`docs/specs/2026-10-11-registration-cli-parity-design.md`.

**Live lane (github.com, 2026-10-12, Linux X64).** Recorded in
`crates/toolu-runner/tests/fixtures/ephemeral_92_live.json`, re-checked
against the GitHub API by `bash scripts/test/ephemeral_92_live_check.sh`
(needs an authenticated `gh`; exits 2 = unverified without it). Ephemeral
toolu runners took real dispatched jobs:
[failure](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38187780731)
(`post-results-live.yml`, runner 169),
[success](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38187819741)
(`noop-live.yml`, runner 170, **no token** at run time so no DELETE was sent)
and [cancelled from GitHub](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38187869097)
(`cancel-live.yml`, runner 171). Each `run` exited 0 after its one job, the
local `config.toml` / `credentials.json` were gone, and `GET
/actions/runners/{id}` returned 404 — for runner 170 without any DELETE from
toolu, which verifies that GitHub removes a used JIT runner itself. Live
`generate-jitconfig` probes (runners 163/164/166/167) showed GitHub stores
exactly the labels sent (no hidden `self-hosted`), rejects `[]` with 422,
de-duplicates case-insensitively — non-ASCII too (runner 172:
`é-92-probe` / `É-92-PROBE` stored once) — and stores the `Linux` label as
`linux`.

**Deleted registration.** The live lane found that a runner deleted on GitHub
makes the OAuth token exchange answer **400** `{"error":"invalid_client"}`
(capture `crates/wire/tests/oauth_invalid_client_400.json`). toolu classified
it as `Protocol` and retried forever (150 s observed, any `run` mode); it is
now `RunnerError::Auth`, as upstream treats `invalid_client`.

| Criterion / scenario | Real input and production path | Expected result | Runnable check |
| --- | --- | --- | --- |
| AC-1, 92-S1 explicit / duplicate / empty | Real `register` binary against a local `generate-jitconfig` endpoint (committed real envelope); request bodies recorded | `--labels gpu` → `[self-hosted, <RUNNER_OS>, <RUNNER_ARCH>, gpu]`; `--no-default-labels --labels gpu,GPU,,tpu` → `[gpu, tpu]`; `--labels ''` → the three defaults; same list in `config.toml` | `cargo test -p toolu-runner --test integration -- register_labels_test::` |
| AC-2, 92-S1 validation | Same binary | `--no-default-labels` alone or with `,,` and a 257-char label → exit 2, upstream message / "256", zero requests, no files | same suite |
| AC-3, 92-S1 no hidden defaults | Live capture `crates/config/tests/fixtures/jit_labels_probe.json` | resolved set == GitHub's stored set (case-insensitive); empty list mirrors GitHub's 422 | `cargo test -p config --test integration -- labels_test::` |
| AC-4 | `register --ephemeral`; pre-#92 `config.toml` text | `ephemeral = true` persisted; missing key loads `false` | `register_labels_test`, `labels_test` |
| AC-5, 92-S2 | Real `run` binary, real RSA JIT envelope, captured `fixtures/job_message.json` running `true` / `exit 3` / `sleep 30` + SIGTERM, local broker + runners API | 1 acquire, 0 `generate-jitconfig`, 1 DELETE, state removed (`_diag/` kept), exit 0; reported `succeeded` / `failed` / `failed` | `cargo test -p toolu-runner --test integration -- ephemeral_run_test::` |
| AC-6, 92-S3 no job / restart | Same harness, idle broker then a job | SIGTERM before a job → exit 0, state kept, 0 DELETE; restarted `run` takes the job and cleans up | `restart_before_the_first_job_keeps_then_uses_the_registration` |
| AC-7, 92-S3 unregister outcomes | Same harness, DELETE 500 / 404 + empty list / no token | 500 → exit 2, state kept, names `toolu-runner remove`; 404 confirmed by lookup → removed; no token → removed, "not verified" | `failed_unregister_keeps_the_registration`, `already_removed_runner_counts_as_unregistered`, `no_token_removes_local_state_and_says_unverified` |
| AC-8 | Pure decision table, incl. "acquired, then completion failed" | every precedence row | `cargo test -p listener --test integration -- loop_decision_test::` |
| Loop edges | Same `run` harness: org URL; one 500 on `/session` then a job; `--once` + 500; `.pending_remove` + 500; live 409 capture on every session POST; `completejob` 400 after the acquire | org → unregistered through `orgs/{org}` (#201), removed; retry → 2 session POSTs, no mint, cleaned; `--once` → exit 1, 1 POST, kept; pending → exit 0, kept; conflict → exit 5 after 8 POSTs, kept; completion failure → cleaned, exit 1 | `ephemeral_run_test` (`org_registration_…`, `transient_failure_…`, `once_with_…`, `pending_remove_…`, `session_conflict_…`, `acquired_job_with_failed_completion_still_cleans_up`) |
| AC-9, 92-S4 | Persisted ephemeral config through the binary | `install-service --print` / `--no-activate` → exit 2, no unit; `--remove` works; `status` prints `mode:` | `cargo test -p toolu-runner --test integration -- install_service_test:: config_resolve_test::` |
| AC-10, 92-S2 live | github.com runs above | run conclusions failure / success / cancelled; toolu exit 0; runners 404; label sets match | `bash scripts/test/ephemeral_92_live_check.sh` |
| AC-12, 92-S3 already removed | Live 400 `invalid_client` capture replayed to `exchange_token` and through the `run` binary; live re-run against deleted runner 168 | `RunnerError::Auth`; `run` exits 1 at once, state kept, 0 DELETE / mint, hint names `toolu-runner remove` | `cargo test -p wire --test integration -- oauth_invalid_client::`; `dead_registration_before_a_job_exits_1_and_keeps_state` |
| AC-11 | Whole workspace | green | `./tools/check.sh all` |

**Mode contract (92-S4).** Every toolu registration is a GitHub JIT runner,
which GitHub removes after one job (verified above). `run` re-mints a new JIT
runner per job; `run --once` exits after one job and leaves the spent local
state; `run` on an `--ephemeral` registration exits after one job and removes
the local state, verifying the GitHub side when a token exists; `boot` runs
one job with nothing persisted. Red evidence: before the change the new
suites failed to compile (missing `config::labels`, `ephemeral`,
`next_ephemeral_action`), and the `invalid_client` replay returned
`Protocol`.

**Differences and unverified.** A SIGTERM mid-job is reported `failed`
(toolu's existing operator-shutdown result, unchanged by #92); a GitHub-side
cancel is reported `cancelled` (live). Upstream exits after an ephemeral job
without calling GitHub; toolu additionally verifies the removal when it has a
token. No official-runner run was repeated for this issue; parity is with the
pinned upstream source plus the live GitHub behavior above. GHES: the same
`generate-jitconfig` / unregister / token-exchange paths serve it, but no
GHES server is available — **unverified**. macOS: the code is
platform-neutral and the tests ran on Linux only — **unverified**.

## Issue #90 — stable per-repository workspaces

Layout and `workspace.clean` follow upstream `PipelineDirectoryManager` /
`TrackingConfig` / `JobExtension` at actions/runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`. The
replays drive the sanitized #68 GitHub.com acquisition
(`crates/execution/tests/incoming_contexts_matrix_0.json`, `__run` step;
only `github.repository` / `repository_owner`, the job id, `workspace.clean`
and the script are rewritten) through the production `Runner` → `run_job`
path. Each script checks out a real local bare origin with real `git`
(clone into an empty workspace, fetch into an existing `.git`). No mocks.
Red evidence: before the implementation all four replays failed. The host
`run:` step had no `GITHUB_WORKSPACE` (`unbound variable`), and
`${{ github.workspace }}` / `${{ runner.workspace }}` rendered empty.

| AC / scenario | Exact observation | Tests | Platforms |
| --- | --- | --- | --- |
| AC-1 / 90-S1 | Two sequential jobs for `alice/app` print the same `GITHUB_WORKSPACE` = `${{ github.workspace }}` = `<work>/app/app` and `RUNNER_WORKSPACE` = `${{ runner.workspace }}` = `<work>/app`. Job 1 clones. Job 2 finds `.git` (same inode as on disk) and fetches commit 2 (`id` = `alice/app@2`). A second repo checked out to `sub/lib` (checkout `path:`) is reused the same way. `_PipelineMapping/alice/app/PipelineFolder.json` has the official runner's camelCase keys `repositoryName`, `pipelineDirectory` `app`, `workspaceDirectory` `app/app` and `repositories.*.repositoryPath`, plus `dataDir` and `lastRunOnUnix`. | `cargo test -p execution --test integration -- stable_workspace_test::stable_workspace_reuses_git_across_jobs_and_fresh_mode_stays_isolated` | Linux x86_64 local; CI `ubuntu-latest` + `macos-14` via `cargo test --workspace` |
| AC-2 / 90-S1 fresh | With `mode = "fresh"`, a third job gets `<work>/<job_id>` for both values (the pre-#90 layout), has no `.git`, and the stable `.git` inode and `HEAD` are unchanged. | same test | as AC-1 |
| AC-3 / 90-S2 | `alice/app`, `bob/app`, `alice/lib` → `<work>/app/app`, `<work>/app_2/app`, `<work>/lib/lib`. None sees another's `.git`, and `rev-parse HEAD` / `id` name each origin. `bob/app` re-runs in `app_2` and moves to commit 2, while `app/app` keeps alice's commit 1. Unit level: a legacy dir with the repo's name, a repo named `_PipelineMapping`, and a claimed name each get a `_<n>` suffix. | `same_repository_name_under_two_owners_and_other_repos_never_collide`; `cargo test -p execution --lib workspace_layout` | as AC-1 |
| AC-4 / 90-S3 | Job A (registration `repo-level`) holds `alice/app`. Concurrent job B (registration `org-level`, same work root) runs in `<work>/<job_id>` and sees only `marker-b`; A sees only `marker-a`. After A ends, job C gets the stable workspace again and sees `marker-a`. Unit level: a busy lease falls back to fresh with an owner-tagged job lease; an absent/unsafe `github.repository` (`../escape`, `a/b/c`) and an odd job id (`id/with:odd` → `id_with_odd`) stay inside the work root. | `concurrent_job_for_a_leased_repository_runs_in_a_fresh_workspace`; `workspace_layout` unit tests | as AC-1 |
| AC-5 / 90-S4 clean | On a populated stable workspace: absent / `true` keep `.git` and the `<work>/app/sibling` dir. `all` removes both. `resources` removes `.git` and keeps the sibling. `outputs` / `OUTPUTS` keep `.git` and remove the sibling. The path stays `<work>/app/app`. A repository's first job (no pipeline dir yet) succeeds with every option, `outputs` included (red before the fix: `WorkspaceInit` / `NotFound`). A corrupt or escaping (`..`) `PipelineFolder.json` is recreated inside the work root. | `workspace_clean_options_follow_upstream`; `every_clean_option_tolerates_a_missing_pipeline_dir`; `corrupt_or_escaping_tracking_file_is_recreated_inside_the_work_root` | as AC-1 |
| AC-6 / 90-S4 GC | Shared root, registration A sweeping. Removed: A's `alice/app` unused for 100 h (its `PipelineFolder.json` too), A's stale unleased fresh dir (and its `_jobs` lease), and a pre-#90 lease-less dir. Kept: A's 1 h-old `alice/lib`, B's stale `bob/tool` and fresh dir, A's live leased `alice/held` and live fresh dir, the running job's dir, a claimed stable dir with a 48 h-old mtime, and `_PipelineMapping`. `stable_gc_after_hours = 0` (default) prunes nothing stable. A pruned repo gets `app/app` back. An unreadable `PipelineFolder.json` claims every name its repo could have: a running job's leased `app` (tracking file overwritten with garbage, 48 h-old mtime) and a lease-less `app_2` stay, while `app_x` and a legacy dir go (red before the fix: `removed` = 4, the leased workspace deleted). | `cargo test -p toolu-runner --test integration -- workspace_gc_test::`; `--test job_teardown_order_test` | as AC-1 |
| AC-7 | A pre-#90 `config.toml` loads as `stable` / never-prune with its `gc_after_hours` kept. `fresh` + `stable_gc_after_hours = 72` round-trips through `save_config`. `mode = "per-job"` fails `load_config` naming the file and the value. `run` / `boot` map `[workspace]` into `RunnerConfig.workspace`. | `cargo test -p config --test integration -- workspace_section_test::`; `--test remint_test`; `cargo test -p shared --test integration -- config_test::` | as AC-1 |
| Job container | With a job container only the workspace is mounted (`/github/workspace`), so `RUNNER_WORKSPACE` and `${{ runner.workspace }}` become its container-side parent `/github`. A fresh workspace (both values the job dir) maps to `/github/workspace`. In a real `ubuntu` container a step sees `RUNNER_WORKSPACE` = `/github` and `GITHUB_WORKSPACE` = `/github/workspace`. Red before the fix: the host path `<work>/hello-world`. | `cargo test -p execution --test integration -- runner_workspace_translation_test::`; `cargo test -p execution --lib docker::job_container -- --ignored` (real Docker) | Linux x86_64 local (Docker 29.1.3); translator test on CI |
| AC-8 | Gate and docs. | `./tools/check.sh all` | Linux local, CI |

Live / reference lane: [`workspace-90.yml`](../.github/workflows/workspace-90.yml)
prints `GITHUB_WORKSPACE`, `github.workspace`, `RUNNER_WORKSPACE`,
`runner.workspace`, whether `.git` existed before `actions/checkout@v4`, and
`PipelineFolder.json`. It runs on GitHub-hosted `ubuntu-24.04` / `macos-15`
(the official runner) and in two sequential `[self-hosted, toolu-90]` jobs,
where the second must find the first job's `.git`. It is dispatch-only;
the toolu jobs run when `run_toolu` is set. (The run below was started by a
since-removed `[live-90]` push trigger on the #90 branch.)

Observed in [run 38177256043](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38177256043)
(head `ceda4f2`). toolu was built from this branch on Linux x86_64 and
registered as `toolu-90-verify` with `[workspace] mode = "stable"`.

| Check | toolu job 1 | toolu job 2 | reference `ubuntu-24.04` / `macos-15` (official runner) |
| --- | --- | --- | --- |
| `GITHUB_WORKSPACE` = `github.workspace` | `<work>/toolu-ghrunner/toolu-ghrunner` | same | `/home/runner/work/toolu-ghrunner/toolu-ghrunner`, `/Users/runner/work/toolu-ghrunner/toolu-ghrunner` |
| `RUNNER_WORKSPACE` = `runner.workspace` | `<work>/toolu-ghrunner` | same | `…/work/toolu-ghrunner` |
| `.git` before `actions/checkout@v4` | no; checkout runs `git init` | yes, inode 3504370 (job 1's); checkout runs `git clean -ffdx` + fetch, no `git init` | no (fresh VM) |
| `PipelineFolder.json` | `repositoryName` / `pipelineDirectory` `toolu-ghrunner` / `workspaceDirectory` `toolu-ghrunner/toolu-ghrunner` | same, newer `lastRunOnUnix` | same keys and values, plus `repositories.*.lastRunOn` and `lastRunOn` (locale format) |

That run is also how the camelCase tracking keys were corrected: an earlier
revision of this branch wrote PascalCase. GHES is **unverified** (no server);
the layout does not depend on the server. A macOS toolu live lane is
**unverified**; macOS real-filesystem behavior is covered by the CI `macos-14`
`cargo test --workspace` run.

Entry paths (90-S3): `run` and `boot` share `run_job`, so both take the
lease. The run `.lock` is not what protects a workspace: an org-level and a
repo-level registration can run the same repository at once on the shared
`~/.toolu-runner/_work`, and that case is AC-4. `boot` has no `.lock`. The
daemon starts each job in its own container with no bind mounts, so its work
roots are isolated. `install-service` only supervises `run`.

Migration: existing `<work>/<job_id>` dirs are left in place and pruned by the
pre-#90 mtime rule. The first stable job for a repository creates its mapping
and skips any existing directory with the same name (suffix).

Limits stated, not claimed: container jobs keep toolu's `/github/workspace`
mount, and `runner.workspace` stays a host path there (upstream mounts `_work`
at `/__w`). Upstream `UpdateRepositoryDirectory` (`internal-set-repo-path`)
is not implemented. Workspaces of a removed registration are never pruned by
other registrations.

## `check` diagnostics subcommand (#93)

`toolu-runner check`, the read-only preflight equivalent to upstream
`run.sh --check` (pinned `cab9d1c`: `src/Runner.Listener/Checks/ActionsCheck.cs`,
`InternetCheck.cs`, `GitCheck.cs`, `NodeJsCheck.cs`). Spec:
`docs/specs/2026-10-12-check-diagnostics-design.md`.

Every hermetic test runs the compiled binary against real loopback sockets
(`tests/helpers/check_servers.rs`: a raw-TCP peer acting as an HTTP target,
a forward proxy, a CONNECT proxy, a 407 proxy, or a peer that never
answers) and asserts on the request heads those sockets actually received.
The registration case uses the committed GHES JIT fixture with its service
URLs rewritten to the local peer. Tool lines are compared with the output
of the same real `git` / `docker` / `node` binaries.

| Criterion / scenario | Real input and production path | Expected result | Runnable check |
| --- | --- | --- | --- |
| AC-1, 93-S1 healthy GHES | `--url http://127.0.0.1:<port>` against a recording peer | `PASS` api / pipelines / token (`/api/v3`, `/_services/{pipelines,vstoken}/_apis/health`), `SKIP` codeload (IP literal), broker (V1), results (GHES); exactly those three GETs; exit 0 | `cargo test -p toolu-runner --test integration -- check_cmd_test::healthy_ghes_target_passes_and_stays_read_only` |
| AC-2, 93-S2 effective hosts | Registration whose JIT `ServerUrl` / `ServerUrlV2` / `AuthorizationUrl` point at distinct local paths, resolved like `run` | the peer receives GETs for exactly the configured paths; header names the registration | `configured_registration_endpoints_are_probed_while_run_holds_the_lock` |
| AC-3, 93-S2 hanging endpoint | Peer that accepts and never answers, `--timeout 1` | `FAIL … timeout`, finishes < 10 s, exit 1 | `hanging_endpoint_is_bounded_by_the_timeout` |
| AC-4, 93-S2 DNS / connect / TLS | `toolu-check.invalid`, a closed port, `https://` to a plaintext peer, a peer that resets the connection | `dns:` / `connect:` / `tls:` FAILs with hints (a reset is `connect:`, never `tls:`), exit 1 | `dns_refused_and_tls_failures_are_classified`, `a_connection_reset_is_classified_as_connect_not_tls` |
| AC-5, 93-S3 authenticated proxy | `HTTP_PROXY=http://user:<secret>@…` to a recording proxy; `NO_PROXY`; a 407 proxy; `REQUEST_METHOD` | proxy receives absolute-form GETs with the Basic `Proxy-Authorization`, target none; `***@` redaction, secret absent from stdout/stderr; `NO_PROXY` and the CGI guard go direct; 407 → `proxy:` FAIL exit 1 | `authenticated_proxy_is_used_and_redacted`, `no_proxy_bypasses_the_proxy`, `proxy_407_fails_the_check`, `cgi_guard_disables_the_proxy_and_is_reported` |
| AC-6, 93-S3 custom CA | `SSL_CERT_FILE` = committed public ISRG Root X1 (`tests/fixtures/check/isrg-root-x1.pem`); a missing path; `NODE_EXTRA_CA_CERTS` | path + "replaces the platform store", `1 loaded`; missing → `0 loaded, 1 errors` + load error; the Node variable echoed | `ssl_cert_file_and_node_extra_ca_certs_are_reported` |
| AC-7, 93-S1 tools | The host's real git / docker / node; empty `PATH`; a `git` that `exec sleep 30`s; `DOCKER_HOST` at an absent socket and with userinfo | exact real versions; `not found on PATH` with exit 0; `timed out after 2 s` within the bound; `daemon unreachable` + endpoint; `tcp://***@…`, secret absent | `cargo test -p toolu-runner --test integration -- check_tools_test::` |
| AC-8, 93-S3 read-only | Empty `TOOLU_RUNNER_HOME`; registration dir with its `.lock` held (fs2 exclusive, as a live `run`) | home stays empty; dir listing unchanged; only `GET`s seen | asserted in AC-1 / AC-2 / AC-13 tests |
| AC-9 | `--url ftp://…`, `--timeout 0`, missing `--config`, `--url` + `--config`, a garbage `jit_config` | exit 2, nothing probed, the garbage never echoed | `usage_and_config_errors_exit_2` |
| AC-10, live | Real github.com / `*.actions.githubusercontent.com` (table below) | six required PASS; ISRG-only `SSL_CERT_FILE` still PASSes and reports the bundled roots | `cargo test -p toolu-runner --test integration --features live -- --ignored check_cmd_test::live_github_com` |
| AC-12, 93-S2 auth | Token path answering 401 | `PASS … HTTP 401`, exit 0 (credentials are never sent or checked) | `unauthenticated_401_is_reachable` |
| AC-13 defaults | Empty home, non-repo cwd, `HTTPS_PROXY` = a CONNECT-recording proxy answering 502 | header `defaults: …no runner registration found…`; CONNECTs to exactly the six github.com service hosts + `nodejs.org`; exit 1; no traffic leaves the host | `defaults_target_the_github_com_hosts` |
| AC-11 | Whole workspace | green | `./tools/check.sh all` |

Pure units (bin crate, `src/tests/check_{env,endpoints,report}.rs`): URL
redaction and credential extraction (schemeless, percent-encoded,
unparseable), hyper-util's proxy precedence and ALL_PROXY fallback, the host
table for github.com / GHE.com / GHES / IP literals / registrations, and the
exit-code aggregation — `cargo test -p toolu-runner --bin toolu-runner check_`.

**Live lane (github.com, 2026-10-12, Linux X64, this worktree's debug
binary).** `toolu-runner check --url https://github.com` → exit 0:

| Probe | URL | Status |
| --- | --- | --- |
| api | `https://api.github.com/` | 200 |
| codeload | `https://codeload.github.com/_ping` | 200 |
| pipelines | `https://pipelines.actions.githubusercontent.com/_apis/health` | 200 |
| broker | `https://broker.actions.githubusercontent.com/health` | 200 |
| token | `https://vstoken.actions.githubusercontent.com/_apis/health` | 200 |
| results | `https://results-receiver.actions.githubusercontent.com/health` | 200 |
| node-dist (optional) | `https://nodejs.org/dist/` | 200 |

git 2.53.0, docker 29.1.3 (CLI and daemon), node v24.21.0 all `OK`; 122
platform trust anchors. Before the User-Agent fix `api` answered 403.

**Finding: trust cannot be narrowed.** Re-running with `SSL_CERT_FILE` set
to the ISRG-only bundle still verified github.com (Sectigo-issued). `cargo
tree -e features -i reqwest` shows `opendal`'s default `rustls-tls`
unifying `rustls-tls-webpki-roots` into every reqwest client, and reqwest
adds `webpki_roots::TLS_SERVER_ROOTS` next to the native anchors. `check`
reports this ("bundled … always trusted too"), the bin now names the
feature explicitly, and the environment variables can add a private CA but
not remove trust — recorded as input for #94.

**Differences and unverified.** No credentials are sent (upstream sends the
`--pat`), so a reachable endpoint passes on any status but 407; upstream's
POST to pipelines, `git ls-remote` and Node HTTPS round trips are not
replicated (spec Non-Goal 4). GHES: derivation verified against local peers
only — no GHES server is available, **unverified** live; the `codeload.<host>`
probe follows upstream and can falsely fail on a GHES without subdomain
isolation. macOS: CI runs the hermetic suites on `macos-14` (no docker CLI
there, so `unreachable_docker_daemon_warns_with_the_endpoint` prints
`UNVERIFIED` and returns); the live lane ran on Linux only — macOS live is
**unverified**. No official-runner comparison run was made; parity is with
the pinned upstream source.

## Strict file commands (#96)

Grammar follows upstream `EnvFileKeyValuePairs`
(`src/Runner.Worker/FileCommandManager.cs`), as built for Linux/macOS, at
actions/runner `cab9d1c3901e45c7705889c4f88284fdd93f4ae5`. Step results merge
as in `ActionRunner.cs:282` / `StepsRunner.cs:348`: the file-command result
merges into the step result before continue-on-error applies.

The replays drive the sanitized #68 GitHub.com acquisition
(`crates/execution/tests/incoming_contexts_matrix_0.json`) through
`Runner::execute_job`. Only scripts, conditions, step inputs,
`continueOnError` and one secret variable are rewritten. Real Bash `printf`,
the host Node runtime and real composite/Node local actions write the files.
No mocks.

Red evidence: run against the pre-#96 engine, the replays fail. Every
malformed body concluded `Success` with no annotations, CR was stripped, and
a missing heredoc terminator swallowed the remainder.

| AC / scenario | Exact observation | Tests | Platforms |
| --- | --- | --- | --- |
| AC-1 / 96-S1, S2 | A later step sees these exact bytes (hex-compared): `EMPTY` = `""`, `EQ` = `b=c`, `MIXED` (`x<<y` after `=`) = `x<<y`, `U` = `héllo ✓`, `CR` = `1\r`, `DUP` = `2`, heredoc `H` = `a\n\nb`, `HE` = `""`, `LIKE` = `EOF2\n EOF`, `HCR` (`N<<EOF\r` … `EOF\r`) = `v\r`. Outputs: `multi` = `l1\nl2`, `out` = `b=c`. Unit level: a heredoc closed at EOF without LF is accepted; BOM stripped; invalid UTF-8 → U+FFFD; a deleted file is empty. | `cargo test -p execution --test integration -- file_command_strict_test::valid_bodies_reach_env_and_outputs_exactly`; `cargo test -p execution --lib file_commands` | Linux x86_64 local; CI `ubuntu-latest` + `macos-14` |
| AC-2 / 96-S3, S4 cleanup | Six bodies each make the writing step conclude `Failure`. Each raises exactly two error annotations and `##[error]` lines: `Unable to process file command 'env' successfully.`, then the reason. The six reasons: `Invalid value. Matching delimiter not found 'EOF'`, `Invalid value. EOF marker missing new line.`, `Invalid format 'just-text'`, `Invalid format '\r'`, `Invalid format '=v'. Name must not be empty`, `Invalid format 'N<<'. Name must not be empty and delimiter must not be empty`. The next `success()` step is skipped, the `always()` step runs, and the job fails. | `malformed_env_fails_the_step_with_upstream_issues_and_runs_cleanup` | as AC-1 |
| AC-3 / 96-S3 scope | One parser for all three files. A script step fails on a bad `GITHUB_OUTPUT` (`'output'`) and a bad `GITHUB_STATE` (`'state'`). A real Node action's main fails on a bad `GITHUB_STATE` (`'state'`), and its post still runs (`NODE96|post|<unset>`). A composite `run:` step's bad `GITHUB_OUTPUT` fails the composite step on the parent record. Its earlier `ok=1` still reaches the action output, and the inner `always()` step runs. A real Docker action's main fails on a bad `GITHUB_ENV` (`'env'`). | `malformed_output_and_state_fail_a_script_step`, `node_action_malformed_state_fails_main_and_post_still_runs`, `composite_run_step_malformed_output_fails_the_parent_step`; Docker: `cargo test -p execution --test integration -- docker_action_linux_test::malformed_env --ignored` with `TOOLU_CONTAINER_TEST_ROOT` | as AC-1; Docker on Linux local (Docker 29.1.3) only |
| AC-4 / 96-S4 | Input `OK=1`, `bad`, `LATER=2` with `continue-on-error: true`. The next step sees `OK=1`, `LATER` unset, and the same step's `GITHUB_OUTPUT` `kept=yes`. `steps.__run.outcome` = `failure`, `conclusion` = `success`, and the job succeeds. | `pairs_before_the_error_apply_and_continue_on_error_keeps_the_job_green` | as AC-1 |
| AC-5 / 96-S4 | `NODE_OPTIONS` and `node_options` written to `GITHUB_ENV` are not applied, while `KEEP=1` is. Each blocked name raises one error annotation (two here), `Can't store NODE_OPTIONS output parameter using '$GITHUB_ENV' command.`, as upstream does per pair, and the step and job stay `Success`. The same holds in a composite `run:` step. Other reserved names are unchanged (upstream blocks only `NODE_OPTIONS`). | `node_options_is_blocked_with_an_issue_but_the_step_succeeds`, `composite_run_step_node_options_is_blocked_without_failing` | as AC-1 |
| AC-6 | A secret job variable inside the bad line is reported as `Invalid format 'bad ***'`. No log line or annotation contains the raw value. | `a_secret_in_a_malformed_line_is_masked_everywhere` | as AC-1 |
| AC-7 | The reference lane below matches on every outcome, value and message. | `.github/workflows/file-commands-96.yml` | GitHub-hosted `ubuntu-24.04`, `macos-15` |
| AC-8 | Gate and docs. | `./tools/check.sh all` | Linux local, CI |

Reference lane (official runner): [run 38206372062](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38206372062),
head `8be7b45`, on GitHub-hosted `ubuntu-24.04` and `macos-15`.

- **Outcomes and values:** the "Assert upstream parity" step passed on
  both images, so every outcome and hex value equals toolu's:
  - `valid` succeeded;
  - `missing_terminator`, `eof_without_newline`, `invalid_line`, `lone_cr`,
    `empty_delimiter`, `bad_output` and `bad_state` each concluded with
    outcome `failure`;
  - `partial` ended `failure`/`success`, with `OK=1`, `LATER` unset and
    `kept=yes`;
  - `node_options` succeeded, with `NODE_OPTIONS` unset and `KEEP=1`;
  - `CR` = `310d` and `HCR` = `760d` (CR kept), `MIXED` = `x<<y`, and
    `LIKE` / `H` / `MULTI` match.
- **Messages:** the check-run annotations carry the same messages:
  - `Unable to process file command 'env'|'output'|'state' successfully.`;
  - `Invalid value. Matching delimiter not found 'EOF'`;
  - `Invalid value. EOF marker missing new line.`;
  - `Invalid format 'just-text'`;
  - `Invalid format 'N<<'. Name must not be empty and delimiter must not be
    empty`;
  - `Invalid format 'bad'`;
  - `Can't store NODE_OPTIONS output parameter using '$GITHUB_ENV' command.`
  
  The lone-CR line shows as `Invalid format ''`, because the annotation view
  drops the CR.

Empty name (`=v`): the official runner **also fails** the step on both
images, but its parser accepts the line (its guard tests the whole line, not
the name). The failure comes later, from .NET's `Value cannot be null.
(Parameter 'name')`. toolu fails at parse time with upstream's own
`Invalid format '=v'. Name must not be empty`. The outcome matches; only the
message text differs.

Not applicable or unverified:

- Windows CRLF splitting (`#if OS_WINDOWS`) does not apply; toolu supports
  only Linux and macOS.
- GHES is **unverified**: the parser does not depend on the server.
- A live toolu self-hosted job is **unverified**: no runner was registered
  for this lane.
- Composite `run:` steps still get no `GITHUB_STATE` file (pre-existing;
  out of scope).
- If a Node stage or composite `run:` step cannot spawn or errors before
  completing, its file commands are not processed. This is pre-existing:
  upstream processes them in a `finally`. Docker stages already read them.
- Only a UTF-8 BOM is stripped. .NET's `File.ReadAllText` also detects
  UTF-16/32 BOMs, which tools writing these files do not emit.
- Two branches are untested: a `GITHUB_PATH` read I/O failure, and a
  blocking-task join error. The reader's I/O-failure branch is
  unit-tested with a directory.

## Node runtime (#95)

Upstream contracts: actions/runner `cab9d1c3901e45c7705889c4f88284fdd93f4ae5`
(v2.337.0) — `HandlerFactory.cs:60-145`, `NodeUtil.cs`, `NodeUtilL0.cs`,
`StepHost.cs::DetermineNodeRuntimeVersion`, `JobExtension.cs` finalize
annotations, `ActionManifestManager.cs:464-507`, `externals.sh` pins
(20.20.2 / 24.21.0, `actions/alpine_nodejs` musl x64). Spec:
`docs/specs/2026-10-12-node-runtime-musl-node24-design.md`.

Red evidence: on the pre-#95 engine the production replay ran the captured
message's node20 action on the node20 runtime and emitted no migration
warning, although the message carries GitHub's live
`actions.runner.requirenode24 = "true"`. `using: node22` parsed as major 22
and silently fell back to node20.

| AC / scenario | Exact observation | Tests | Platforms |
| --- | --- | --- | --- |
| AC-1 / 95-S2 | All 27 `[InlineData]` rows of upstream's `NodeUtilL0.cs`, committed verbatim with line provenance (`crates/execution/tests/node_migration_upstream_l0.json`), select the same version and warning presence. Declared node12/16 → node20 rules, node24 untouched; exact warning (`Both … in the {workflow,system} environment … node20/node24.`) and phase-2 info text; .NET `bool.TryParse` flags (`" TRUE "` true, `"1"`/`"yes"` false), `ConvertToBoolean` switches (`1`/`true`/`$true`), an empty workflow value overriding a true system value, and no warning for cross-source conflicts. Flags parse from the captured github.com message. | `cargo test -p execution --lib node::migration` | Linux x86_64 local; CI `ubuntu-latest` + `macos-14` |
| AC-2 / 95-S2, S4 host | Production `Runner::execute_job` replay of the captured `job_outputs_message.json` with the committed probe `.github/actions/node-runtime-95` (`using: node20`): `process.execPath` is the `node/24.21.0` runtime and the "Complete job" row carries exactly `Node.js 20 is deprecated. The following actions target Node.js 20 but are being forced to run on Node.js 24: ./node action. …`. With `requirenode24` set to `"false"` (wire structure kept) and step env `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION: true`, it runs on `node/20.20.2` and the only warning is upstream's deprecation text with the message's dates. With both switches true in step `env` (phase 2), the stage step gets upstream's same-source warning as a `##[warning]` line plus a warning annotation, and the phase-2 notice as a log line. | `cargo test -p execution --test integration -- node_migration_test::` | as AC-1 |
| AC-3 / 95-S1 | This host classifies as `linux-x64 (glibc)` from its real loader, the `ci-macos` host as `darwin-arm64`. Loader rule over roots: Alpine layout → musl; glibc loader + Debian's `ld-musl` → glibc; dangling absolute loader symlink (Ubuntu shape) → glibc. Errors: `Node.js actions are not supported on linux arm` (armv7, FreeBSD, Windows), `JavaScript Actions on musl (Alpine) hosts are only supported on x64 Linux runners. Detected Linux Arm64`. Docker lane: `docker export`ed pinned `alpine` → musl, `debian` → glibc, `debian` + `apt-get install musl` → glibc. | `cargo test -p execution --lib node::platform`; `cargo test -p execution --test integration -- node_container_test::exported --ignored` | as AC-1; export lane Linux local (Docker 29.1.3) |
| AC-4 / 95-S4 container | The probe runs in real job containers through the production path, replaying the captured github.com job-container acquisition (`toolu-runner/tests/fixtures/job_container_message.json`, live `requirenode24`; only its `jobContainer` image literal swapped for a pinned digest): pinned `alpine` → `v24.x`, no glibc, `/github/node/24.21.0-alpine/bin/node` (real `actions/alpine_nodejs` download); pinned `ubuntu` → `v24.x`, glibc, `/github/node/24.21.0/bin/node`. Upstream's probe output (`ID=alpine`, `ID=ubuntu\nID_LIKE=debian`) is pinned; an Alpine container on arm64 fails with upstream's `JavaScript Actions in Alpine containers are only supported on x64 Linux runners. Detected Linux Arm64`. A cancelled or timed-out probe is an error and is not cached as glibc. | `cargo test -p execution --test integration -- node_container_test:: --ignored --test-threads=1` (CI Docker step); `node::platform` | Linux x86_64 local + CI Docker step |
| AC-5 / 95-S3 preseed | `TOOLU_RUNNER_NODE_DIR` with the real host `node` as `node{M}/bin/node` resolves with the download base pointed at a dead port (no network) and leaves the cache untouched; a missing `node24_alpine` fails with `… not found in TOOLU_RUNNER_NODE_DIR: expected <path>`; the host node filed under another major and a script answering `v99.0.0` fail the start check with exact messages; a proven preseed is not re-run per stage (memoized by path, major, mtime). Job containers get it read-only at `/github/externals` (volume and path translation asserted); a missing dir is neither created nor mounted. The `TOOLU_RUNNER_NODE_DIR` accessor treats empty as unset and makes a relative path absolute; its one-line wiring into the `run` / `boot` configs is covered by review only. | `cargo test -p execution --lib node::runtime`; `cargo test -p execution --lib docker::container_mounts`; `cargo test -p toolu-runner --bin toolu-runner config::tests::node_dir` | as AC-1 |
| AC-6 / 95-S3 downloads | Over a real local HTTP server: a tarball holding the real host `node` commits only after `node --version` passes (run with an empty environment); one whose `bin/node` answers `v99.0.0` is rejected before commit with the exact message (no cache dir, no staging leftovers); 404 (`status 404 Not Found`), non-gzip and truncated real bodies (`extract node tarball:`) leave nothing; the alpine `./bin/node` layout extracts with no strip; an entry escaping the runtime dir is rejected; concurrent promotes leave one tree. Network lane: real nodejs.org 24.21.0 installs validated; a host-targeted install of the real `actions/alpine_nodejs` binary commits only where it starts — refused on a glibc host without `ld-musl` (CI ubuntu), validated on this Debian-`musl`-package host — while the container-targeted install always commits it. | `cargo test -p execution --lib node::`; `cargo test -p execution --test integration -- node_download_test:: --ignored` | as AC-1; network lane Linux x86_64 local |
| AC-7 | `using: node22`/`node18`/`Node26`/`node` → `'using: X' is not supported, use 'docker', 'node12', 'node16', 'node20' or 'node24' instead.`; `Node20`, `NODE16` accepted. | `cargo test -p execution --lib actions::manifest` | as AC-1 |
| AC-8 | Gate and docs. | `./tools/check.sh all` | Linux local, CI |

Reference lane (official runner): `.github/workflows/node-runtime-95.yml`
runs the same probe on GitHub-hosted `ubuntu-24.04`, `macos-15` and an
`alpine` job container —
[run 38217199303](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38217199303),
head `f3f92ff`, runner 2.337.0, all three jobs `success`:

- `ubuntu-24.04`: `{"version":"v24.19.0","execPath":".../externals/node24/bin/node","glibc":"2.39"}`;
- `macos-15`: `{"version":"v24.19.0","execPath":".../externals/node24/bin/node","glibc":null}`;
- `alpine` container: `{"version":"v24.19.0","execPath":"/__e/node24_alpine/bin/node","glibc":null}`
  — the official runner picks the musl build for Alpine, as toolu does;
- every job's "Complete job" annotation is exactly toolu's text, sorted the
  same way: `Node.js 20 is deprecated. The following actions target Node.js 20
  but are being forced to run on Node.js 24: ./.github/actions/node-runtime-95,
  actions/checkout@v4. For more information see: https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/`.

The hosted 2.337.0 images ship Node 24.19.0, while toolu pins 24.21.0 from
`externals.sh` at the pinned commit `cab9d1c`. The patch level differs; the
selected major and the musl/glibc choice match.

Unverified: a live musl (Alpine) **host** and Linux arm64 hosts (classified
from exported roots and the error paths only); toolu on a live acquired job
(the replays use captured messages); GHES (flags arrive as the same job
variables; not exercised). Not applicable: Linux ARM32 deprecation/kill
switches (toolu ships no armv7 build — such a host gets the unsupported-CPU
error) and container hooks.

## Compatibility matrix and reference lane (#103)

The epic-wide record is [compatibility-matrix.md](compatibility-matrix.md); the
method is [compatibility-evidence.md](compatibility-evidence.md). This section
lists the #103 test surfaces.

| Surface | Test / command | Lane |
| --- | --- | --- |
| Matrix integrity (ids, lanes, evidence, Markdown view) | `python3 scripts/test/compat_matrix_check.py --check`; `scripts/test/test_compat_matrix_check.py` mutates the real matrix for every rejection rule | CI "Compatibility evidence (#103)" |
| Paired toolu / official v2.337.0 run | [run 38301583727](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38301583727) → `docs/compat-reference-103.json`; `reference_compare.py --verify` + `test_reference_compare.py` | recorded live; re-verified in CI |
| S1 — dropped message sections fail the production replay | `crates/execution/tests/replay_mutation_test.rs` (captured `compat_103_contexts_message.json` + #70 capture) | default gate (ubuntu + macOS) |
| S2 — real container action and exact input | `crates/execution/tests/compat_probe_replay_test.rs` (captured `compat_103_docker_message.json`, real Docker; echo-instead-of-container and empty `who` fail) | CI Docker step |
| Capture hook | `crates/listener/src/tests/capture.rs` (exact body, 0600/0700, contained name, unwritable dir) | default gate |
| Sanitizer | `scripts/test/test_sanitize_capture.py` (real envelope with injected probe secrets) | CI |
| Honest live lane | `scripts/test/live_lane_test.sh`; `live.yml` fails `UNVERIFIED` without its secret | CI (static) |

The paired run recorded two differences, kept unverified in the matrix:
forwarder-mode cache never enables cache service v2 (save silently fails,
restore 400), and the official runner reports a `Pull <image>` setup step toolu
does not. Details: [compatibility-evidence.md#recorded-differences](compatibility-evidence.md#recorded-differences).
## Proxy and custom CA (#94)

Optional `[network]` (`proxy`, `no_proxy`, `ca_bundle`) applied to every
runner HTTP client through `network::client_builder`, and exported to steps.
Spec: `docs/specs/2026-10-12-explicit-proxy-and-custom-ca-design.md`;
upstream reference `RunnerWebProxy.cs`, `HostContext.cs`, `ContainerInfo.cs`
(pinned `cab9d1c`).

Every peer is real and generated at test time
(`crates/network/tests/helpers/tls_fixtures.rs` and `proxy_fixtures.rs`, `#[path]`-included by
the other crates): an rcgen CA with valid / expired / wrong-host leaves, a
tokio-rustls HTTPS target recording request lines, and a tokio `CONNECT`
proxy that requires Basic auth, records each request line with its
`Proxy-Authorization`, and tunnels whatever host was asked for to
`127.0.0.1` (portable; no `127.0.0.2`). No committed keys, no mocked
transport: requests travel real reqwest / hyper / rustls stacks, real bash,
real Node and real Docker.

| Criterion / scenario | Real input and production path | Expected result | Runnable check |
| --- | --- | --- | --- |
| AC-1 config schema | `config.toml` text from the README example with and without `[network]` | trimmed `no_proxy`, `~`-expanded `ca_bundle`, redacting `Debug`; absent → env mode and no table written back | `cargo test -p config --test integration -- network_section_test::`; `[network]` preserved by re-mint: `cargo test -p config --test integration -- remint_test::` |
| AC-2, 94-S2 authenticated proxy + bypass | `client_builder` → auth `CONNECT` proxy → HTTPS target (`target.test`); `no_proxy` destination pinned to loopback with `ClientBuilder::resolve` | proxy sees `CONNECT target.test:<port>` + `Basic base64(u:p@ss)` (percent-decoded); a plain `http://` destination goes in absolute form with the same header (and gets 407 without credentials); `http://:pw@` still sends Basic auth; unencoded `#` / `/` / `?` in a password is a redacted config error; bypass target reached directly, proxy log empty | `cargo test -p network --test integration -- client_proxy_test::` |
| AC-3, 94-S1 precedence | env `HTTPS_PROXY`/`NO_PROXY` naming a second recording proxy or a closed port (`temp-env`) | config proxy carries the request, env proxy and env `NO_PROXY` ignored; config `no_proxy` direct even with a dead env proxy; env mode keeps the env proxy; config mode reaches `127.0.0.1` directly | same file |
| AC-4, 94-S3 trust | generated CA as `ca_bundle`; expired leaf; leaf for `other.test`; bundle relative / missing / directory / empty / text / key-only / valid+malformed / non-certificate DER; two-CA bundle | 200 with the bundle (one request recorded); without it `UnknownIssuer`, and with it `Expired` / `NotValidForName` for the two bad leaves, none reaching the server; `RunnerError::Config` naming the path for every bad bundle; two certificates counted | `cargo test -p network --test integration -- client_tls_test::` |
| AC-5, 94-S1/S4 host export | captured #68 acquisition replayed through `Runner::execute_job`: a bash `run:` step, an `ACTIONS_RUNNER_HOOK_JOB_STARTED` hook and a local composite action's `run:` step printing the variables; a step `env:` override; env-mode run | proxy in four variables (the composite step sees the same values as the plain step), `NO_PROXY`/`no_proxy` = list + loopback, `NODE_EXTRA_CA_CERTS` = bundle, `TOOLU_RUNNER_*` stripped, the runner's own `HTTPS_PROXY` overridden; step env wins; env mode re-exported in both casings | `cargo test -p execution --test process_env -- network_step_env_test::`; unit level `cargo test -p network --test integration -- step_env_test::` |
| AC-6, 94-S3/S4 Node | local `node20` action (main + post), run on Node 24 as the captured message's `requirenode24` demands (#95), with both runtime cache slots seeded from the host `node`; the same action under a workflow `env: HTTPS_PROXY`; `node -e https.get` against the generated-CA server | both stages see `HTTPS_PROXY`; the workflow value wins over the export; 200 with `ca_bundle`, a certificate error code without (and no request reaches the server) | same file (`node_action_main_and_post_see_the_export`, `node_action_sees_the_workflow_env_over_the_export`, `node_trusts_the_ca_bundle_only_when_configured`) |
| AC-7, 94-S4 containers | real Docker: job container (its own `HTTP_PROXY` added), a `run:` step exec'd into it, an nginx service whose health check asserts its env, two alpine Docker actions running `env` (one with a step `env:` override) | proxy variables in both casings; container / service / step values kept; no `NODE_EXTRA_CA_CERTS` anywhere | `cargo test -p execution --test integration -- network_container_env_test:: --ignored --test-threads=1` (CI Docker lane); without Docker, `cargo test -p execution --lib job_http` (`container_env_keeps_explicit_values_and_never_gets_the_host_ca_path`) and `--lib action_container` (`step_env_proxy_wins_over_the_export`, `exports_proxy_vars_in_both_casings_without_the_host_ca_path`) |
| AC-8, 94-S2/S3 live log | tokio-tungstenite `wss://` server over tokio-rustls behind the auth proxy; a plain HTTPS server answering 200 | handshake via `CONNECT`, `Authorization: Bearer …` and the log frame received; untrusted CA → `None`; non-101 answer, a wrong or a missing `Sec-WebSocket-Accept` → `None`; `live_log_client` refuses a missing bundle; `ws`/`wss` URL mapping table | `cargo test -p wire --test integration -- live_log_proxy_test::`; `cargo test -p wire --lib live_log` |
| AC-9, 94-S2 secrets | step `echo "$HTTPS_PROXY"` with `http://u:s3cr3tpw@…` through the engine; `proxy_secrets` / `describe` units | the job masker (shared with the tracing redactor and the journal writer) renders `http://***@…`; password raw + decoded, `user:password` and `base64(user:pass)` registered, the bare username not; a scheme-less env value (`alice:s3cretpw@proxy.corp:3128`) masked; a value with an unencoded `/ ? #` in the password masked whole; `Debug` shows `<set>`; redaction never echoes a mis-parsed value (`user:pa://ss@…`, `…?y=http://z`) | `an_echoed_proxy_password_is_masked`; the production journal writer fed the same job's events: `cargo test -p toolu-runner --test integration -- network_journal_mask_test::` (no password, no `base64(u:s3cr3tpw)`, `***@` present); `cargo test -p network --test integration -- proxy_env_test:: secrets_test::`; `cargo test -p shared --test integration -- network_config_debug_test::` |
| 94-S1 env mode = reqwest | real reqwest clients built under `HTTPS_PROXY=` + `https_proxy=<proxy>`, under `REQUEST_METHOD` + `HTTPS_PROXY`, and under an unparseable `HTTPS_PROXY` + `ALL_PROXY` | the first two go direct (recording proxy sees nothing) and `describe` / the step export agree (`proxy none`, no dead proxy exported); the third tunnels via `ALL_PROXY` and the export names it | `cargo test -p network --test integration -- proxy_env_test::` |
| `[network]` hardening | `register --replace` of a registration with a missing `ca_bundle` / with a proxy (real binary, local `generate-jitconfig`); `ca_bundel` typo; `ActionFetcher` built from an unusable `[network]` | replace fails closed (exit 2, no mint sent, config untouched) / keeps `[network]` in the rewritten config; unknown key rejected by name; both fetcher constructors fail closed (`ArchiveClient` is the only, no-redirect, archive client) | `cargo test -p toolu-runner --test integration -- register_replace_network_test::`; `cargo test -p config --test integration -- network_section_test::`; `cargo test -p execution --test integration -- network_job_clients_test::` |
| AC-10, 94-S1/S3 CLI | real `toolu-runner check --config` with `[network]` → auth proxy → GHES-shaped HTTPS target, env `HTTPS_PROXY` at a dead port; missing `ca_bundle` for `check` and `run --once` | `PASS api` through the proxy (`CONNECT ghes.test:<port>`), `[network]` line redacted with the certificate count, env-vars-ignored note, password absent, exit 1 only because the fixture JIT's `pipelines` / `token` hosts are unreachable; exit 2 naming the path (missing bundle) or `[network] proxy` (`ftp://x`), nothing probed | `cargo test -p toolu-runner --test integration -- network_cli_test::`; repeat-pass log/secret memo: `cargo test -p toolu-runner --bin toolu-runner network_setup` |
| AC-11 one builder | every `crates/*/src` file; a real tar.gz action archive through the auth proxy; an `L2Tier` put to `https://s3.test` | no raw `reqwest` construction outside `crates/network/src`; archive extracted via `CONNECT`, refused without the bundle (sanitized `ArchiveTransport` error, origin never reached, cause `UnknownIssuer`); the job client and the accelerated upstream client each send `CONNECT` + Basic auth and get the body; S3 `PUT` arrives via `CONNECT s3.test:<port>` | `cargo test -p toolu-runner --test integration -- network_client_inventory_test::`; `cargo test -p execution --test integration -- network_job_clients_test::`; `cargo test -p execution --lib job_http`; `cargo test -p cache --test l2_network_test` |
| AC-12 docs | README "Proxy and custom CA", this section, AGENTS.md | present | review |
| AC-13 | whole workspace; `cargo tree -p protocol` | green; no `reqwest` in `protocol` | `./tools/check.sh all`; CI "Protocol crate is sync + I/O-free" |

**Applicability and unverified lanes.** Linux X64 for everything above;
the macOS CI job runs the hermetic suites (no Docker there, so AC-7 is
Linux-only). **Unverified:** a real corporate proxy and TLS-intercepting CA,
live github.com and GHES through a proxy (GHES uses the same clients and no
GHES-specific transport, but no GHES server is available), the S3 tier
against real S3 through a proxy (the tier is not wired into production yet),
and a side-by-side comparison with the official runner. Upstream parity
differences, by design: config mode replaces env `NO_PROXY` instead of
merging it; upstream reads lower-case variables first, reqwest (and so env
mode here) upper case first; `NODE_EXTRA_CA_CERTS` is new (upstream exports
no CA).

## GitHub App registration (#201)

`register --app` mints the JIT config with a GitHub App installation token and
records `[auth] kind = "app"`. Plain `register` does the same when
`github-app.json` names the URL's host and no personal token is given. `run`
then re-mints after every job with a cached or fresh installation token. Spec:
`docs/toolu/specs/2026-10-23-github-app-installation-token-design.md`
(gitignored).

Real data:
- Shell-out tests run the built binary in temporary homes against loopback
  peers.
- The peers replay live 401/404 captures (2026-10-23) and GitHub's documented
  success examples from `crates/toolu-runner/tests/fixtures/github_app/`. Each
  file records where it came from, and the example token is sanitized.
- App JWTs are verified with the public half of a test key that is registered
  to no App (`crates/protocol/tests/fixtures/app_jwt_test_key.pem`).

| Criterion / scenario | Real input and production path | Expected result | Runnable check |
| --- | --- | --- | --- |
| S1 App JWT | `build_app_jwt_at` with the test key; tampered and non-PEM inputs | RS256, `iat` −60 s, `exp` +540 s, `iss` = App id; verifies with the public key; key errors never echo the PEM | `cargo test -p protocol --test integration -- app_jwt_test::` |
| S2 register | Real `register --app` binary; loopback installation, `access_tokens` and `generate-jitconfig` | installation found, token minted, JIT minted with `Bearer <installation token>`, `[auth] kind = "app"` persisted; not installed → exit 2 naming where it is installed | `cargo test -p toolu-runner --test integration -- register_app_test:: github_app_wire_test::` |
| S3 / AC-1 re-mint | Real `run` binary through two jobs | re-mint with the cached installation token; other sections byte-identical | `cargo test -p toolu-runner --test integration -- run_app_remint_test::` |
| S4 / AC-2 failures | Live 401 / 404 `Integration not found` captures, a 503 | 401/404 fatal with the App's fix; 503 backs off | same suite |
| AC-3 token registrations | pre-#201 `config.toml` text; token `register` / re-mint | a config without `[auth]` loads as token and saves without one; re-mint unchanged | `cargo test -p config --test integration -- registration_auth_test::` |
| AC-4 masking | App PEM, JWTs and installation tokens through every durable sink; a 401/403 whose body echoes the bearer at `register` and at a fatal re-mint | none reaches `_diag` or the journal; the stderr error shows `Bearer ***` | `cargo test -p toolu-runner --test process_env -- secret_sink_coverage_test::`; `register_app_test::a_rejected_app_prints_an_echoed_jwt_masked`, `run_app_remint_test::a_rejected_re_mint_prints_an_echoed_app_jwt_masked`, `run_app_remint_test::a_rejected_mint_prints_an_echoed_installation_token_masked` |
| S5 live, personal repo | **pending**: the throwaway App supplied for this check | register, job, re-mint, second job | live run on github.com |
| S5 live, organization | **unverified**: `create-app` makes a private (`public: false`) App owned by the user, which GitHub lets only the owning account install | org runner registers and re-mints with an App installation token | moved to #202 (its live S4) |
