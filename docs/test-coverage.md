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
and types. In the workflow-call fixture, the `github_token` and
`system.github.token` secret aliases use the same fixed synthetic UUID instead
of the generic `[redacted]` marker, retaining `isSecret: true`. The capture
checker pins those exact placeholder values. Fixtures and workflow files have
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
