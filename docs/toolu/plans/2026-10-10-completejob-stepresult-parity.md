# CompleteJob / StepResult payload parity (#88) — Plan

**Date:** 2026-10-10   **Status:** Approved   **Spec:** docs/toolu/specs/2026-10-10-completejob-stepresult-parity-design.md   **Topic:** environmentUrl, billingOwnerId, infrastructureFailureCategory, upstream StepResult shape, Complete job row

## Evidence and approach

Inspected: `listener/src/job_lifecycle/completion.rs` (request build),
`listener/src/step_reporter.rs` (`StepCollector`),
`listener/src/execution_loop.rs` (forwarder; 499 code lines),
`execution/src/execution/job_runner.rs` + `job_runner/outputs.rs` (#70
end-of-job outputs), `job_spec::evaluate_acquired_outputs` (mask check),
`step_attrs.rs` (typed token evaluation), `steps_runner.rs` / `post_drain.rs`
(numbering), `action_exec.rs` / `docker_action.rs` / `handlers/shell_command.rs`
(handler kind), `actions/{download_info,downloader,prefetch}.rs` (fetch errors),
`composite_exec.rs` (nested reporting), `observability/src/journal/types.rs`,
the #82 recording-server test (`listener/src/tests/annotation_reporting.rs`,
`support.rs`) and the #99 live lane (`.github/workflows/step-attrs-99.yml`).
Upstream evidence in the brainstorm. Approach exactly as the approved spec.

Live lane: `completejob-88.yml` triggers on pushes to this branch only and
its jobs run only when the head commit message contains `[live-88]`, so
ordinary pushes queue nothing. The toolu lane uses a locally registered
runner labelled `toolu-88`; the reference lane is GitHub-hosted
`ubuntu-24.04`. Captures (S5) use the current toolu build plus an uncommitted
acquire-body dump.

## Workstream summary

size headroom → wire shape → shared types → journal → live capture →
URL evaluation → Complete job step → step metadata → typed fetch failures →
listener completion → live verification → docs → gate.

## Steps (machine-readable)

```json
[
  {
    "id": "S1-outage-extract",
    "title": "Move apply_outage_override + LOST_CONNECTION_MESSAGE from execution_loop.rs to listener/src/outage_override.rs (no behavior change)",
    "check": "cargo test -p listener watchdog && bash scripts/guardrails/run.sh",
    "paths": [
      "crates/listener/src",
      "crates/listener/Cargo.toml"
    ],
    "model": "inherit"
  },
  {
    "id": "S2-wire-shape",
    "title": "wire: StepResult upstream snake_case keys, StepState/JobConclusion strings, action_name/ref/type, annotations always sent, no outcome; CompleteJobRequest environment_url/billing_owner_id/infrastructure_failure_category (None omitted); update every StepResult/CompleteJobRequest constructor so the workspace compiles",
    "check": "cargo test -p wire --test completejob_wire_test && cargo check --workspace --all-targets",
    "ac_refs": [
      "AC-3",
      "AC-4"
    ],
    "input": "upstream StepResult.cs / CompleteJobRequest.cs member names (pinned cab9d1c); boundary: None fields omitted, empty annotations serialized as []",
    "paths": [
      "crates/wire",
      "crates/listener",
      "crates/toolu-runner/tests"
    ],
    "model": "inherit"
  },
  {
    "id": "S3-shared-types",
    "title": "shared: ActionsEnvironment + billingOwnerId on AgentJobRequestMessage; RunnerEvent::StepMetadata / InfrastructureError / JobCompleted.environment_url; RunnerError::ActionFetch(ActionFetchError) + infrastructure_category(); pass-through arms and constructors across the workspace (journal mapping stubs land in S4)",
    "check": "cargo test -p shared && cargo check --workspace --all-targets",
    "ac_refs": [
      "AC-1",
      "AC-4"
    ],
    "input": "crates/execution/tests/job_outputs_message.json (billingOwnerId U_kgDOAH-IyQ, actionsEnvironment null); ActionFetchKind -> category table",
    "paths": [
      "crates"
    ],
    "depends_on": [
      "S2-wire-shape"
    ],
    "model": "inherit"
  },
  {
    "id": "S4-journal-mapping",
    "title": "observability: step_metadata journal line type and InfrastructureError -> annotation mapping, unit tests in journal/tests",
    "check": "cargo test -p observability",
    "ac_refs": [
      "AC-8"
    ],
    "paths": [
      "crates/observability",
      "crates/shared/src/events.rs"
    ],
    "depends_on": [
      "S3-shared-types"
    ],
    "model": "inherit"
  },
  {
    "id": "S5-capture",
    "title": "Add .github/workflows/completejob-88.yml (URL, secret-URL and all-step-kinds jobs; toolu-88 + ubuntu-24.04 lanes; runs only when the head commit message has [live-88]); register the toolu-88 runner; capture + UUIDv5-sanitize three acquired messages into crates/execution/tests/completejob_88_{env,secret,steps}_message.json; create completejob_88_evidence.json (capture section) and the checker. If registration or the run is impossible, stop and report needs-human (S6-S10 depend on real captures).",
    "check": "python3 scripts/test/completejob_evidence_check.py --local",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-6"
    ],
    "input": "github.com acquired job messages from the [live-88] capture run; --local verifies UUIDv5 sanitization, non-empty actionsEnvironment/billingOwnerId on the env capture, mask hints on the secret capture, and fixture SHA-256s against the evidence JSON",
    "paths": [
      ".github/workflows/completejob-88.yml",
      ".github/actions",
      "crates/execution/tests/completejob_88_env_message.json",
      "crates/execution/tests/completejob_88_secret_message.json",
      "crates/execution/tests/completejob_88_steps_message.json",
      "crates/listener/tests/completejob_88_evidence.json",
      "scripts/test/completejob_evidence_check.py"
    ],
    "depends_on": [
      "S4-journal-mapping"
    ],
    "model": "inherit"
  },
  {
    "id": "S6-environment-url",
    "title": "execution: environment_url.rs \u2014 restricted context (no secrets/inputs), scalar->string, mapping/sequence/expression errors, mask check, warning text; every boundary row of the spec table",
    "check": "cargo test -p execution --test environment_url_test",
    "ac_refs": [
      "AC-1",
      "AC-2"
    ],
    "input": "completejob_88_env_message.json / completejob_88_secret_message.json with real bash; capture-derived boundary variants",
    "paths": [
      "crates/execution/src",
      "crates/execution/tests/environment_url_test.rs",
      "crates/execution/tests/completejob_88_env_message.json",
      "crates/execution/tests/completejob_88_secret_message.json"
    ],
    "depends_on": [
      "S5-capture"
    ],
    "model": "inherit"
  },
  {
    "id": "S7-complete-step",
    "title": "execution: job_runner/complete_step.rs \u2014 Complete job row (v4 UUID, number = highest reported + 1 via ExecutionContext note_step_number), outputs warning moved with upstream text, URL evaluation, step vs job conclusion (cancelled stays cancelled), JobCompleted.environment_url; tests incl. cancelled run still sends URL and Complete job numbered last",
    "check": "cargo test -p execution && cargo test -p listener job_outputs && cargo check --workspace --all-targets && bash scripts/guardrails/run.sh",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-5"
    ],
    "input": "completejob_88_env_message.json with a real sleep step cancelled via the job token; job_outputs_message.json",
    "paths": [
      "crates/execution",
      "crates/listener/src/tests/job_outputs.rs",
      "crates/toolu-runner/tests"
    ],
    "depends_on": [
      "S6-environment-url"
    ],
    "model": "inherit"
  },
  {
    "id": "S8-step-metadata",
    "title": "execution: emit StepMetadata for run (ShellCommand name), node{major}, composite, Dockerfile, docker:// (DockerHub), pre and post stages (Docker/network tests print a distinct UNVERIFIED marker when the prerequisite is absent)",
    "check": "mkdir -p /tmp/toolu-docker-88 && TOOLU_CONTAINER_TEST_ROOT=/tmp/toolu-docker-88 cargo test -p execution --test step_metadata_test -- --include-ignored --test-threads=1 && cargo test -p execution && cargo check --workspace --all-targets",
    "ac_refs": [
      "AC-3"
    ],
    "input": "completejob_88_steps_message.json with real bash, node and docker (Docker 29.1.3 present; Docker cases #[ignore]-gated and run here with --include-ignored)",
    "paths": [
      "crates/execution",
      "crates/toolu-runner/tests",
      ".github/actions"
    ],
    "depends_on": [
      "S7-complete-step"
    ],
    "model": "inherit"
  },
  {
    "id": "S9-fetch-failures",
    "title": "execution: ActionFetch typed kinds at source sites (download_info, download_info_v1, downloader, extract_tarball), stream flag, is_archive_auth_error on ArchiveStatus, InfrastructureError emission from step_errors and the composite reporter",
    "check": "cargo test -p execution && cargo check --workspace --all-targets && cargo test -p toolu-runner --test execute_job_error_reporting_test",
    "ac_refs": [
      "AC-4"
    ],
    "input": "fault-injection axum servers (labelled as such: 500/422/403/404/401x2/401-then-200/mid-body cut) + a real corrupt gzip archive + a real valid action tarball; nested composite and continue-on-error cases",
    "paths": [
      "crates/execution",
      "crates/shared/src/error.rs",
      "crates/toolu-runner/tests"
    ],
    "depends_on": [
      "S8-step-metadata"
    ],
    "model": "inherit"
  },
  {
    "id": "S10-listener-completion",
    "title": "listener: collector applies metadata + infra annotations + failed-step category latch; Set up job metadata; forwarder latches environment_url; JobOutcome/report_completion fill environmentUrl, billingOwnerId (parse-failure fallback), infrastructureFailureCategory; recording-server tests (environment_url, secret_environment_url, step_metadata, infrastructure_category, completejob_ retry/cancel, billing_owner)",
    "check": "cargo test -p listener && cargo test -p listener step_metadata -- --include-ignored && bash scripts/guardrails/run.sh",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "input": "completejob_88_*_message.json through Runner::execute_job and wire::net::complete_job into RecordingServer",
    "paths": [
      "crates/listener",
      "crates/execution/src",
      "crates/wire/src",
      "crates/shared/src",
      "crates/execution/tests/completejob_88_env_message.json",
      "crates/execution/tests/completejob_88_secret_message.json",
      "crates/execution/tests/completejob_88_steps_message.json",
      ".github/actions"
    ],
    "depends_on": [
      "S9-fetch-failures"
    ],
    "model": "inherit"
  },
  {
    "id": "S11-journal-fixture",
    "title": "Regenerate the canonical journal fixture from a real engine run (JOURNAL_CAPTURE=1 capture_canonical) now that StepMetadata/Complete job events exist; assert step_metadata lines and an infrastructure annotation line (AC-8); a separate writer test (fault-injected action fetch failure through the real engine) asserts the infrastructure annotation line",
    "check": "cargo test -p toolu-runner --test journal_writer_test --test journal_reader_test --test journal_types_test",
    "ac_refs": [
      "AC-8"
    ],
    "paths": [
      "crates/toolu-runner/tests/fixtures/journal/canonical.jsonl",
      "crates/toolu-runner/tests/journal_writer_test.rs",
      "crates/toolu-runner/tests/journal_reader_test.rs",
      "crates/toolu-runner/tests/journal_types_test.rs",
      "crates/observability",
      "crates/execution/src"
    ],
    "depends_on": [
      "S10-listener-completion"
    ],
    "model": "inherit"
  },
  {
    "id": "S12-live-verify",
    "title": "Run completejob-88.yml ([live-88]) with the branch-head toolu build; record deployments/jobs/check-run API JSON in completejob_88_evidence.json. Default checker mode is green when every required lane is passed or recorded unverified with a reason (GHES and macOS allowed-unverified); --strict (non-gating) demands all lanes passed. Remove the toolu-88 runner afterwards. The checker prints per-lane status and `AC-6: UNVERIFIED` loudly when any required lane is not passed; the PR body quotes it.",
    "check": "python3 scripts/test/completejob_evidence_check.py",
    "ac_refs": [
      "AC-6"
    ],
    "input": "github.com run of completejob-88.yml on toolu-88 + ubuntu-24.04",
    "paths": [
      "crates/listener/tests/completejob_88_evidence.json",
      "scripts/test/completejob_evidence_check.py",
      ".github/workflows/completejob-88.yml"
    ],
    "depends_on": [
      "S11-journal-fixture"
    ],
    "model": "inherit"
  },
  {
    "id": "S13-docs",
    "title": "docs: test-coverage #88 section + #99 note, README reporting, architecture completion payload, crate READMEs (new module rows), AGENTS.md Key Modules",
    "check": "python3 scripts/test/completejob_evidence_check.py --docs && bash scripts/guardrails/run.sh",
    "ac_refs": [
      "AC-7"
    ],
    "input": "--docs asserts the #88 section in docs/test-coverage.md with every AC row, the resolved 'Complete job' note, and README rows for complete_step.rs, environment_url.rs, outage_override.rs",
    "paths": [
      "docs/test-coverage.md",
      "README.md",
      "docs/architecture.md",
      "AGENTS.md",
      "crates/execution/src/execution/README.md",
      "crates/execution/src/execution/job_runner/README.md",
      "crates/execution/src/execution/actions/README.md",
      "crates/listener/src/job_lifecycle/README.md",
      "crates/listener/src/tests/README.md",
      "crates/wire/src/reporting/README.md",
      "crates/observability/src/journal/README.md",
      "scripts/test/completejob_evidence_check.py"
    ],
    "depends_on": [
      "S12-live-verify"
    ],
    "model": "inherit"
  },
  {
    "id": "S14-gate",
    "title": "Full gate",
    "check": "./tools/check.sh all",
    "ac_refs": [
      "AC-7"
    ],
    "paths": [
      "crates",
      "docs",
      "scripts",
      ".github",
      "AGENTS.md",
      "README.md",
      "tools"
    ],
    "depends_on": [
      "S13-docs"
    ],
    "model": "inherit"
  }
]
```

## Critical files

Create: `crates/listener/src/outage_override.rs`,
`crates/execution/src/execution/environment_url.rs`,
`crates/execution/src/execution/job_runner/complete_step.rs`,
`crates/wire/tests/completejob_wire_test.rs`,
`crates/execution/tests/{environment_url_test,step_metadata_test,action_infrastructure_test}.rs`,
`crates/listener/src/tests/completejob.rs`, `crates/execution/tests/step_metadata_test.rs`,
`crates/execution/tests/completejob_88_{env,secret,steps}_message.json`,
`crates/listener/tests/completejob_88_evidence.json`,
`scripts/test/completejob_evidence_check.py`, `.github/workflows/completejob-88.yml`.

Modify: `crates/wire/src/reporting/{types,run_service}.rs`,
`crates/shared/src/{events,error}.rs`, `crates/shared/src/job_message/request.rs`,
`crates/observability/src/journal/types.rs`, execution files listed in S7-S9,
`crates/listener/src/{step_reporter,execution_loop,setup_forwarding,step_report_queue,job_lifecycle}.rs`,
`crates/listener/src/job_lifecycle/completion.rs`, docs in S13.

## Verification

End to end: captured github.com messages → real engine (bash/node/docker) →
`StepCollector` → production `wire::net::complete_job` → recording server
body assertions (AC-1..5), plus the paired live run against the hosted
official runner (AC-6). Failure/boundary rows of the spec's table each have a
named test. Docs are synchronized in S13; S14 runs the whole gate. Any lane
without its prerequisite (Docker, network, GHES, macOS) is recorded
unverified, never passed.

## Delivery

- Commit after each green step with a conventional subject (`feat(…)`,
  `test(…)`, `docs(…)`, `refactor(…)`). Pushes before review happen only for
  recoverability checkpoints and the `[live-88]` capture (S5) and
  verification (S12) runs, which GitHub triggers from a push; the
  post-review push follows the audit below.
- Expensive checks run through the epic wrapper:
  `bun /root/.claude/plugins/cache/toolu/epic-orchestrator/7.11.0/scripts/job.ts -- -- <cmd>`
  (bun consumes the first `--`).
- Docker-gated tests run here with `--include-ignored` (Docker 29.1.3,
  Node 24 present); their output is recorded in the PR.
- S5/S12 need the GitHub API (authenticated `gh`, repo admin) and a toolu
  runner registered from this host. S5 without captures stops with
  needs-human; S12 records an unavailable lane as unverified with a reason.
- The evidence checker is not wired into `tools/check.sh`/CI, matching the
  existing `*_evidence_check.py` scripts; S12/S13 run it explicitly.
- Before the post-review push: `plan-ledger.js run <plan> --verify`,
  `toolu-review:review`, `verdict.js status` = ready; then the PR
  (`Closes Falconiere/toolu-ghrunner#88` / `Part of …#67`) and
  `pr-babysit:babysit`.

## Review log

Adversarial plan review (independent reviewer, two rounds): round 1 → narrow
paths on compile-wide steps, journal fixture before events existed,
unsatisfiable strict live check, capture fallback, ignored Docker tests, docs
content check, delivery push order; round 2 → wider regression checks on
S7-S9, explicit AC-8 infrastructure-line test, loud AC-6 status, stale step
numbers. All applied; no blockers remained.

## Deviations

- S4: no separate `observability` unit test file; the new journal mapping is
  asserted in `toolu-runner/tests/journal_types_test.rs` (round trip,
  `ref` key, infrastructure → `error` annotation, URL not journaled), which
  already owns the per-variant contract.
- S7: "Complete job" always ends with `Job conclusion: <c>` so the row has a
  log (`finalize_split` requires every completed row to carry a log URL); a
  failed job-started hook keeps ending the job in "Set up job" without a
  "Complete job" row (spec Non-Goal 7). Existing tests that count workflow
  rows now skip the "Complete job" row by name.
- S8: the Docker rows need the daemon-shared `TOOLU_CONTAINER_TEST_ROOT`
  (same as CI's Docker step); the check now sets it.

- Pre-push review fixes:
  - A post skipped by `post-if` carries no metadata, since upstream labels a row only when its handler runs. This is covered by `gh_compat_prepost`, which is red without the fix.
  - A local job's output error now closes the "Complete job" row as failed.
  - The `Job conclusion:` line applies the runner-shutdown override.
  - Output names and evaluation errors are masked.
  - The token discriminants are shared from `job_spec`.
  - New listener tests cover first-failure-wins, the masked infrastructure message and the billing echo on a parse failure.
  - The evidence checker now:
    - compares the toolu lane against the reference lane, with documented allowlists;
    - requires the exact warning and a deployment status for the secret job;
    - never lets an `unverified` entry stand in for a required lane.
  - CI runs `--local`/`--docs`.
  - An internal runner error that aborts the step loop still ends the job without "Complete job", which is now a documented known difference.
- Re-review fixes: the environment-URL error text is masked too. New unit tests in `execution/tests/complete_step.rs` cover row closure on a local output error and the masking of names and errors; the masking tests are red without the fix. `runner_shutdown_is_the_conclusion_complete_job_logs` pins that the row's `Job conclusion:` line agrees with a shutdown-failed job. A shutdown already fails the steps, so that test does not discriminate the `after_shutdown` guard, which stays as a consistency guard.
