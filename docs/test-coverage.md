# Test Coverage Map

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
| 3   | Multi-step job (run, uses, docker://)            | `crates/toolu-runner/tests/live_e2e.rs::multi_step_job` (gated; runs checkout + setup-node + docker://alpine).                                                                                                                                                                                                                                                    | live         |
| 4   | Action resolution (`actions/checkout@v4`)        | `crates/toolu-runner/tests/live_e2e.rs::action_resolution` (gated), `crates/toolu-runner/tests/actions_resolver_test.rs` (planned; see `crates/execution/src/execution/actions/resolver.rs::parse_action_ref` and `resolve_action_refs` for the parser-level coverage).                                                                                                  | live         |
| 5   | Expression evaluation (`${{ }}` full syntax)     | `crates/toolu-runner/tests/expression_eval_test.rs` — 28 tests covering literals, context/property/index access, function calls (`contains`, `startsWith`, `endsWith`, `format`, `join`, `toJson`, `fromJson`, `success`, `failure`, `always`, `cancelled`), binary ops, unary `!`, wildcards, and parse errors.                                                       | default      |
| 6   | Secret masking in logs                           | `crates/toolu-runner/tests/secret_masker_real_test.rs` — 10 tests driving the real `SecretMasker` against recorded log fixtures (`tests/fixtures/secret-masking-input.txt` → `secret-masking-expected.txt`), JSON-escaped variants, multi-line secrets, longest-first replacement, and the full `RedactingWriter` pipeline.                                                | default      |
| 7   | OIDC token issuance                              | `crates/toolu-runner/tests/oidc_token_test.rs` — 6 tests spinning up the real `OidcServer` axum endpoint on `127.0.0.1:0`, posting to `/_apis/pipeline/oidc/requestToken`, decoding the returned JWT, verifying the default and overridden audiences, the 10-min expiry, and bearer-token auth.                                                                       | default      |
| 8   | Artifact upload + download                       | `crates/execution/src/execution/artifacts/` (types only; v1 artifact service lives in `execution::execution::artifacts`). Live flow gated on artifact end-to-end tests landing in step 10.                                                                                                                                                                       | out-of-scope |
| 9   | Cache save + restore                             | `crates/cache/src/` (types only; v1 cache service lives in the `cache` crate). Live flow gated on cache end-to-end tests landing in step 10.                                                                                                                                                                                      | out-of-scope |
| 10  | Reusable workflows                               | `crates/toolu-runner/tests/reusable_workflow_test.rs` — 23 tests covering `parse_reusable_ref` (with simple, nested, and SHA git_refs), `validate_inputs` / `resolve_inputs` (required + default + caller-override), `validate_secrets` (Inherit + Explicit modes), `resolve_outputs` (job output mapping), `check_nesting_depth` (4-level limit), `check_circular_reference`.  | default      |
| 11  | Composite actions                                | `crates/toolu-runner/tests/composite_action_test.rs` — 14 tests parsing a real-shape composite `action.yml` (with `inputs`, `outputs`, `runs.using: composite`, multi-step `steps[]`), preparing composites (scope + depth tracking, depth limit), evaluating `${{ steps.X.outputs.Y }}` output expressions, and handler dispatch (composite / script / node / docker / unknown).   | default      |
| 12  | Reconnect after transient network failure        | `crates/toolu-runner/tests/net_test.rs` — `exchange_token_returns_protocol_error_on_http_failure` (401) and `poll_message_returns_none_on_202` (long-poll no-work). Listener-side exponential backoff in `crates/listener/src/job_lifecycle.rs` (covered by the listener smoke test). Live mid-job outage scenarios in the design spec are tracked in `docs/known-bugs.md`. | default      |
| 13  | Concurrent single-job guarantee (file lock)      | `crates/toolu-runner/tests/failure_modes_test.rs::lock_acquire_writes_body_and_releases_on_drop`, `lock_conflict_returns_held_pid`, `lock_replaces_stale_lock_when_holder_pid_dead`. Live two-process race in `crates/toolu-runner/tests/live_e2e.rs::concurrent_single_job` (gated).                                                                                  | default      |
| 14  | Cancel by GH (UI cancel → 30s)                   | `crates/toolu-runner/tests/live_e2e.rs::cancel_by_github` (gated; dispatches a `sleep 300` job, calls `POST /actions/runs/{id}/cancel`, asserts `conclusion: cancelled` within 60s).                                                                                                                                                                              | live         |
| 15  | GHES V1 protocol                                 | `crates/toolu-runner/tests/ghes_v1_test.rs` — 6 tests driving `wire::net::v1::{fetch_connection_data, post_timeline_record, fetch_timeline}` against a `wiremock` server returning the real V1 service-discovery shape, plus the `protocol::v1::resolve_service_url` URL resolver exercised end-to-end.                                                          | default      |
| 16  | Service install (launchd + systemd)              | `install.sh` (single shell script that emits a launchd plist on macOS, a systemd unit on Linux). Manual smoke only — no automated test for the service files.                                                                                                                                                                                                   | out-of-scope |
| 17  | Install script                                   | `install.sh` itself; executable + shellcheck'd manually.                                                                                                                                                                                                                                                                                                       | out-of-scope |
| 19  | Lint + typecheck gate                            | `lefthook.yml::pre-commit::rust-clippy` runs `cargo clippy --workspace --all-targets -- -D warnings`; `tools/check.sh all` runs `cargo fmt --all -- --check` + clippy + file-size + no-allow + no-unwrap. The `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy` invocations in the build pipeline are the live gate.            | out-of-scope |
| 20  | Live bug tracking                                | `docs/known-bugs.md` is the single source of truth; updated when the listener smoke test or live tests surface a behavior gap.                                                                                                                                                                                                                                | out-of-scope |
| 21  | Protocol crate is sync + I/O-free                | `crates/protocol/Cargo.toml` dep list is restricted to `serde*`, `base64`, `jsonwebtoken`, `num-bigint-dig`, `pkcs1`, `sha1`, `sha2`, `aes`, `cbc`, `uuid` (no `reqwest`, `tokio`, `opendal`, `bollard`, `axum`). `crates/protocol/tests/integration.rs` is `#[test]` only — no `#[tokio::test]`, no `wiremock`.                                                                  | default      |
| 23  | Real-data test suite                             | This file. The 6 new test files (`expression_eval_test.rs`, `secret_masker_real_test.rs`, `oidc_token_test.rs`, `reusable_workflow_test.rs`, `composite_action_test.rs`, `ghes_v1_test.rs`) plus the recorded fixtures under `tests/fixtures/` form the suite. Live tests under `tests/live_e2e.rs (harness: tests/helpers/)` complete the picture with `--features live`.                    | default+live |

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

(The multi-step workflow — `checkout@v4` + `setup-node@v4` + a docker-pull step — is embedded inline in `crates/toolu-runner/tests/live_e2e.rs::multi_step_job`, not a fixture file.)

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
step_context_identity_test` and `cargo test -p listener --lib
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
| AC-1, 71-S1 | Workflow `bash`/`defaults-71-workflow`, then job `sh`/`defaults-71-job`; real shell identifies `sh` and writes only `defaults-71-job/job.marker`. A capture-derived partial job layer clears the workflow shell. | `cargo test -p execution --test job_message_run_defaults_test job_defaults_override_workflow` and `later_partial_mapping_clears_earlier_shell`; passing Linux production replay. |
| AC-2, 71-S1 | Captured explicit step uses Bash and `defaults-71-step`; `explicit.marker` exists there, not in the job directory. An empty explicit shell falls through to job `sh`. | `step_values_override_defaults`, `empty_explicit_shell_uses_job_shell`; passing Linux production replay. |
| AC-3, 71-S2 | A capture-derived absent-default job writes `root.marker` at the job workspace root. Relative job path, step path with spaces, and `${{ runner.temp }}` absolute path run in their selected directory. A nonexistent default cwd fails the correct step and its log names the full path. | `paths_and_absence`, `nonexistent_default_directory_fails_its_step_with_path`; passing Linux production replay. Host-shell fallback parity remains with #80. |
| AC-4, 71-S3 | The captured local composite writes `.defaults-71-composite.marker` at the workspace root; the next top-level `sh` step reads it from its job-default directory. | `composite_scope`; passing Linux production replay. Composite's own cwd behavior belongs to #81. |
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
| S1 / failed post result | Main `Success`, distinct post `Failure`, job `Failure`; completion JSON uses conclusion `3`, main `2`, post `3`, number `3`, and `Post Workflow input passed to action`. | `post_failure_reaches_job_completed`; `post_results_reach_distinct_completion_records`; `cargo test -p execution --test post_results_test post_failure_reaches_job_completed && cargo test -p listener --lib post_results_reach_distinct_completion_records` | Local production replay; GitHub UI unverified. |
| S2 / hard error drain | A/B mains then B missing-script failure followed by `A:post` with A-state and A input; four completion records, final Failure. | `hard_post_error_keeps_draining_lifo`; `cargo test -p execution --test post_results_test hard_post_error_keeps_draining_lifo` | Local production replay. |
| S3 / live conditions | After B post failure, A `failure()` runs and A `success()` skips. After A main failure, B `failure()` runs and B `success()` skips. A hard B main error still completes after A `failure()` cleanup. Cancelling a running main keeps `Cancelled` even when A's eligible post fails. | `post_conditions_follow_live_status`, `hard_main_error_still_completes_after_post_cleanup`, `cancelled_main_keeps_failure_post_and_final_cancelled`; `cargo test -p execution --test post_results_test post_conditions_follow_live_status && cargo test -p execution --test post_results_test hard_main_error_still_completes_after_post_cleanup && cargo test -p execution --test post_results_test cancelled_main_keeps_failure_post_and_final_cancelled` | Local production replay; pinned reference unverified. |
| S4 / state and report isolation | B post reads B-state/B/`__self_2` before A reads A-state/A/`__self`; four distinct report IDs. | `repeated_posts_keep_state_and_report_identity`; `cargo test -p execution --test post_results_test repeated_posts_keep_state_and_report_identity` | Repeated top-level local production replay; nested pending #102. |
| S5 / cancellation budget | B's observed post start precedes cancel; A's later `cancelled()` post finishes, B never writes a late finish marker, job `Cancelled`, completion within 5 s. A 120 ms shared deadline reaches zero after elapsed time. | `cancelled_posts_complete_job`, `cancel_budget_is_shared`; `cargo test -p execution --test post_results_test cancelled_posts_complete_job && cargo test -p execution --lib cancel_budget_is_shared` | Local real processes plus deadline unit check; multi-post expiry unverified. |

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

Run `cargo test -p execution --test action_downloads_test` and
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
| AC-1 / S1 | `format`, `contains`, bracket access, shell expression, `github.action_path` in step `env`, and output mapping produce `hello world`, `world/42`; malformed syntax and forbidden `secrets.*` emit a parent error instead of an empty string. | `expressions_render_functions_brackets_conditions_and_outputs`, `malformed_run_expression_fails_step_then_runs_cleanup`, `forbidden_context_fails_visibly_then_runs_cleanup`; `cargo test -p execution --test composite_semantics_test` | Captured-job macOS replay passes; expression-valued composite cwd awaits #81. |
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
## Job containers (#73)

Local component evidence uses real Docker output and actual shell/action code.
`job_container_test.rs` and `job_container_linux_test.rs` drive `Runner::execute_job`
using the existing message fixture with explicit test steps/declarations. This is
production execution coverage, **not** an acquired container-message capture.
No parser or constructed fixture result establishes live backend parity.

| Scenario | Test surface and exact observable | Status |
| --- | --- | --- |
| 73-S1 | `execution/tests/job_container_linux_test.rs`: Ubuntu OS, shared hostname across shell/Node/composite/posts, container cwd and exact marker bytes | Local Linux lane; see command below |
| 73-S2 | `docker/tests/job_container.rs`: paths with spaces, metadata equals inspect, ephemeral published port, user bind-volume persistence, multiline opaque env preserved, cleanup; `job_container_composite_test.rs`: container shell, expression paths and ENV/PATH; Linux lane: OUTPUT/ENV/PATH/STATE | Local Docker; live checkout/artifact backend remains unverified |
| 73-S3 | Service aliases and Docker actions sharing the job network | Unverified; requires #74/#75 integration |
| 73-S4 | `docker/tests/job_container_failures.rs`: observed-start cancellation, timeout/post exec, pull/create/start/exec failure, owned-resource removal | Explicit real-Docker lane |
| 73-S5 | `execution/tests/job_container_test.rs`: non-Linux declaration fails before host workspace/step, absent declaration keeps host behavior | Default macOS lane; Linux setup-cancel conclusion regression |
| Live applicability | Captured `jobContainer`, official pinned runner comparison, real GitHub artifact bytes, GHES | Unverified; no closure claim |

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
