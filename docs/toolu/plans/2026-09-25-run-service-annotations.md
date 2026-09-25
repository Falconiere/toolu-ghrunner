# Run Service annotations

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/toolu/specs/2026-09-25-run-service-annotations-design.md   **Topic:** Issue #82 completion annotations and evidence.

## Evidence and approach

Issue #82 and epic #67 require numeric upstream annotation levels, full command location/title data, per-step completion placement, real outgoing POST evidence, GitHub UI comparison, and a green gate. The pinned official runner's `Annotation.cs`, `AnnotationLevel.cs`, `IssueExtensions.cs`, and `ActionCommandManager.cs` define the fields and normalization. Current `command_parser` parses but `command_dispatch` drops range/title; `StepCollector` ignores annotations; the listener's only completion annotation is the outage watchdog. Reuse the captured `incoming_contexts_matrix_0.json` job shape and the existing real-shell production replay pattern. Keep the journal v1 line format stable. #84 owns matchers; #88 owns other completion metadata. At planning time no self-hosted runners were registered; a 2026-09-25 repository API recheck also found zero registered runners. No GHES endpoint is configured. A missing live lane is unverified and blocks acceptance rather than making a checker green.

## Workstream summary

Pin command/wire behavior with real-tool tests → collect by report step and capture the outgoing POST → add paired live workflow/checker → synchronize docs and run gate → execute required remote comparison.

## Steps (machine-readable)

```json
[
  {
    "id": "command_contract",
    "title": "Test then carry upstream command annotation fields, range normalization and masking into engine events",
    "ac_refs": ["AC-1", "AC-2"],
    "depends_on": [],
    "paths": ["crates/execution/src/execution/command_parser.rs", "crates/execution/src/execution/command_dispatch.rs", "crates/shared/src/events.rs", "crates/execution/tests/command_annotation_test.rs", "crates/execution/tests/command_annotation_probe.sh", "crates/execution/tests/incoming_contexts_matrix_0.json"],
    "input": "Sanitized captured acquired job and real shell workflow commands for full, absent, end-only, invalid, descending, multiline and secret-bearing annotations",
    "check": "cargo nextest run -p execution --test command_annotation_test"
  },
  {
    "id": "completion_route",
    "title": "Attach commands to the enclosing StepResult and verify production completejob POST bytes",
    "ac_refs": ["AC-1", "AC-3"],
    "depends_on": ["command_contract"],
    "paths": ["crates/listener/src/step_reporter.rs", "crates/listener/src/execution_loop.rs", "crates/listener/src/setup_step.rs", "crates/observability/src/journal/types.rs", "crates/wire/src/reporting/run_service.rs", "crates/wire/src/net/run_service.rs", "crates/wire/src/reporting/types.rs", "crates/listener/src/tests/annotation_reporting.rs", "crates/listener/src/tests/watchdog_trip.rs", "crates/toolu-runner/tests/journal_types_test.rs", "crates/execution/src/execution/composite_exec.rs", "crates/execution/src/execution/composite_shell.rs", "crates/execution/src/execution/composite_env.rs", "crates/execution/tests/command_annotation_probe.sh", "crates/execution/tests/incoming_contexts_matrix_0.json"],
    "input": "Captured job with real shell and nested local composite output; loopback receiver records completejob POST; outage regression supplies job-level annotation",
    "check": "cargo nextest run -p listener -E 'test(annotation_reporting) | test(watchdog_trip)' && cargo nextest run -p toolu-runner --test journal_types_test"
  },
  {
    "id": "live_assets",
    "title": "Add runnable paired-run workflow and fail-closed Checks API verifier",
    "ac_refs": ["AC-4"],
    "depends_on": ["completion_route"],
    "paths": [".github/workflows/annotation-82-live.yml", ".github/actions/annotation-82-live/action.yml", ".github/annotation-82-source.txt", "crates/toolu-runner/tests/annotation_live.rs", "Cargo.toml", "crates/toolu-runner/Cargo.toml"],
    "input": "One branch-push workflow SHA on dedicated toolu-82-tool and toolu-82-reference labels; GitHub Checks API annotations with expected severity/title/path/range, followed by Checks UI step association",
    "check": "cargo nextest list -p toolu-runner --features live --test annotation_live"
  },
  {
    "id": "docs_gate",
    "title": "Document behavior, evidence status and platform/backend applicability; run the full gate",
    "ac_refs": ["AC-5"],
    "depends_on": ["completion_route", "live_assets"],
    "paths": ["README.md", "docs/test-coverage.md", "docs/toolu/specs/2026-09-25-run-service-annotations-design.md", "docs/toolu/plans/2026-09-25-run-service-annotations.md", ".github/workflows/annotation-82-live.yml", "crates"],
    "input": "Final branch files and #82 matrix with local passing results versus unverified remote lanes",
    "check": "./tools/check.sh all"
  },
  {
    "id": "live_acceptance",
    "title": "Run paired GitHub.com and supported GHES Checks/UI comparisons and record actual evidence",
    "ac_refs": ["AC-4"],
    "depends_on": ["docs_gate"],
    "paths": [".github/workflows/annotation-82-live.yml", "crates/toolu-runner/tests/annotation_live.rs", "docs/test-coverage.md", "README.md"],
    "input": "Provisioned equivalent toolu and pinned official self-hosted runners; GH_TOKEN and a supported GHES endpoint/token; same workflow/action SHA",
    "check": "cargo nextest run -p toolu-runner --features live --test annotation_live --run-ignored only -j 1"
  }
]
```

## Critical files

`crates/shared/src/events.rs`, `crates/execution/src/execution/command_parser.rs`, `crates/execution/src/execution/command_dispatch.rs`, `crates/execution/tests/command_annotation_test.rs`, `crates/execution/tests/command_annotation_probe.sh`, `crates/wire/src/reporting/types.rs`, `crates/listener/src/step_reporter.rs`, `crates/listener/src/execution_loop.rs`, `crates/listener/src/setup_step.rs`, `crates/listener/src/tests/annotation_reporting.rs`, `crates/observability/src/journal/types.rs`, `crates/toolu-runner/tests/journal_types_test.rs`, `crates/toolu-runner/tests/annotation_live.rs`, `.github/workflows/annotation-82-live.yml`, `README.md`, `docs/test-coverage.md`. Include only necessary compile-site test updates.

## Verification

Run each ledger check and inspect the exact outgoing JSON and Checks API result. The local recording receiver proves POST bytes, not GitHub rendering. The live tests must fail on missing host, credentials, runner label, incorrect pin, UI mismatch, or absent annotation; no skipped/ignored test is passing evidence. Use a real browser to inspect each run's Checks page after the API assertions, recording a UI link and screenshot or equivalent capture. Record host OS/architecture, GitHub.com or GHES version, runner and workflow revisions, run/Checks URLs, and source fixture provenance in `docs/test-coverage.md`. A registered self-hosted runner must expose a verified binary version; a label alone is not pin evidence. After a scoped commit, run local review/verdict and the full gate; re-fetch/rebase and repeat the gate if main moved. When paired runners and GHES access are available, push only `feat/82-send-annotations-in-the-run` to trigger the workflow at the committed SHA, run `live_acceptance` and branch-wide `--verify`, then open the authorized PR to `main` with the required `Closes`/`Part of` lines and hand it to babysit. If a required live prerequisite cannot be supplied after code and local checks are ready, report `needs-human` with the exact missing prerequisite and do not call the AC complete.

## Deviations

- The session's pre-tool hook denies direct `cargo test` commands. Focused test and live checks use `cargo nextest` instead. `./tools/check.sh all` remains the unchanged repository gate and invokes its own required `cargo test --workspace` layer.
- Real composite replay exposed that composite shell stdout bypassed the workflow-command dispatcher. The execution step now includes `composite_exec`, `composite_shell`, and `composite_env` so nested commands reach the same parser and preserve stdout/file-output precedence.
- GitHub's documented `workflow_dispatch` rule requires the workflow file on the default branch. The issue branch uses a branch-scoped `push` trigger for its two fixed labels; no push to `main` is authorized. The live verifier finds that push run at the checked-out SHA.
- A later full-gate run failed while extracting a Node test runtime because the host system volume holding `/tmp` had only 418 MiB free (`os error 28`); the worktree volume had 1.4 TiB free. The two affected composite tests passed with `TMPDIR` on the worktree volume. A first `target/tmp` attempt made the repository-inference test see its temp directory inside the checkout; moving `TMPDIR` to `/Volumes/Projects/.herdr/tmp/toolu-ghrunner-82` outside the checkout made both the inference and composite cases pass. The unchanged gate is rerun under that environment; no test or gate layer is skipped.
- The branch-wide ledger includes `live_acceptance`, whose input is the workflow run created by the feature-branch push. Local gate/review precede that push; live acceptance and branch-wide verification follow it once the paired runners and GHES access exist. This corrects the initial verification paragraph's impossible ordering.
