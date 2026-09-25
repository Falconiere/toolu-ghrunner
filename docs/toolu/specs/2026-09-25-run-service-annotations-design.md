# Run Service annotations — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** Issue #82, workflow-command annotations in `completejob`

## Problem

`::error`, `::warning`, and `::notice` currently produce `RunnerEvent::Annotation`, but the listener discards that event when it builds `CompleteJobRequest`. The wire `Annotation` also uses `annotationType/file/line/col` rather than the Run Service contract. GitHub therefore cannot reliably show a command annotation or link it to its step and source range.

## Non-Goals

1. Problem matcher discovery and output matching belong to #84. Matchers may use the annotation path added here later.
2. Environment URLs, billing owner, infrastructure category, and action name/ref/type in `StepResult` belong to #88. This issue adds the per-step annotation field required by #82; #88 can reuse it.
3. Results Service step-update or GHES V1 timeline annotation formats are not changed. The supported acquired-job completion path is Run Service `completejob`; GHES applicability must be established by a real GHES run.

## Architecture

Use the pinned [official runner `Annotation`/`AnnotationLevel` contracts](https://github.com/actions/runner/tree/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Sdk/RSWebApi/Contracts), [`IssueExtensions.ToAnnotation`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Sdk/RSWebApi/Contracts/IssueExtensions.cs), and [`ActionCommandManager.ValidateLinesAndColumns`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/ActionCommandManager.cs) as the behavioral oracle. Extend `WorkflowCommand` → `RunnerEvent::Annotation` with title and range data. The dispatcher validates partial/descending ranges before emission, unescapes command properties, and masks every string field at its producer boundary. The listener's `StepCollector` attaches events by the emitted report step id, using `StepStarted.step_number` for `stepNumber`. Composite shell stdout must pass through that same dispatcher; the real replay exposed that it previously emitted only logs. Its dispatcher uses the enclosing report step id. A collector must retain multiple annotations and also handle an annotation observed before or after its completion event without losing it.

`StepResult.annotations` carries command annotations, matching the official runner's task-record path. `CompleteJobRequest.annotations` remains for job-level runner failures, including the outage watchdog. Choosing only the top-level list would lose the required per-step association. The ordinary `RunnerEvent` channel still carries annotation data for journal consumers; the Run Service payload is built from the collector before `completejob`. The local integration check sends this request through the production `wire::net::complete_job` function to a recording HTTP receiver and asserts its received bytes. That proves the outgoing transport payload; separate GitHub service/UI runs prove backend rendering.

## Interfaces / Schema

`wire::reporting::Annotation` serializes the upstream names `level`, `message`, `title`, `rawDetails`, `path`, `isInfrastructureIssue`, `startLine`, `endLine`, `startColumn`, `endColumn`, `stepNumber`. `level` is numeric: `UNKNOWN=0`, `NOTICE=1`, `WARNING=2`, `FAILURE=3`; workflow `Error` maps to `FAILURE`. The command path has no `rawDetails` or infrastructure flag. Omit absent/default fields as upstream `EmitDefaultValue=false` does; preserve `message` and nonzero `level`. When present, the range and step numbers are signed 64-bit wire values, matching upstream `long` (the command parser accepts upstream 32-bit signed numeric inputs). `StepResult` gains optional `annotations: Vec<Annotation>` serialized as `annotations` when nonempty. The existing top-level `annotations` array remains.

For a valid command with `file=src/main.rs,line=4,col=2,endLine=4,endColumn=8,title=Probe`, the corresponding step result contains `{ "level": 3, "message": "...", "title": "Probe", "path": "src/main.rs", "startLine": 4, "endLine": 4, "startColumn": 2, "endColumn": 8, "stepNumber": <reported step number> }`. Property `%25/%0D/%0A/%3A/%2C` escapes decode once; message data uses only its data-escape set. Secret masking applies after decoding to message, title, and path.

## Failure modes and edge cases

- Blank or whitespace-only annotation messages are not sent, as upstream `ToAnnotation` returns null. A command with no file or range still reports its message/severity and step association.
- Use the upstream validation order: an end line alone becomes the start line; an end column alone becomes the start column; columns without a line are removed; columns for a multi-line range are removed; a descending line pair is removed; a descending column pair is removed. Unparseable numbers are absent. Missing end values default to the start value. Zero/default values are omitted from JSON. Keep a valid message even when a range is invalid.
- A nested composite annotation uses the enclosing report step id/number, not its synthetic inner id. Multiple annotations from one step preserve event order. A missing `StepStarted` mapping cannot invent a step number: preserve the annotation on the matching result if one exists, otherwise warn and omit it from completion rather than silently assigning it to another step.
- The existing masker must redact all annotation strings before journal, serialized payload, diagnostics, or UI sinks. A mask added during the same step applies to later commands. Any transport error follows the existing completion retry/failure behavior; no annotation may be silently removed to make a retry succeed.
- `rawDetails` and `isInfrastructureIssue` are represented in the wire type for compatibility but remain absent for ordinary workflow commands. A true infrastructure flag on a job-level runner annotation is preserved where its producer supplies one.

## Acceptance criteria

- **AC-1:** Given real `::error`, `::warning`, and `::notice` shell output with escaped title/path, full range, and a disposable secret, the acquired-job completion payload contains upstream field names and numeric levels 3/2/1, decoded and masked strings, and the correct reported step number, with no legacy keys.
- **AC-2:** Given real commands with absent, partial, invalid, descending, multi-line, and multiline-message inputs, the completion payload retains valid annotations and applies the pinned upstream range/default/omission behavior without leaking a secret.
- **AC-3:** Given multiple commands emitted by a real nested composite action and an ordinary step, each appears once on its enclosing `StepResult.annotations` in order, including when completion timing crosses event collection; job-level outage annotations remain in the top-level list.
- **AC-4:** Given the committed annotation workflow running on the toolu runner and pinned official runner with the same revision, GitHub.com's Checks UI and API show matching severity, title, path/range and step association; a supported GHES run proves its applicable completion path. Missing host/credentials or skipped lanes are recorded as unverified.
- **AC-5:** The repository gate passes, and user documentation plus the coverage map identify the changed behavior and distinguish local replay from live UI/reference evidence.

## Acceptance evidence

| AC / scenario | Real input and expected observation | Runnable check / evidence |
| --- | --- | --- |
| AC-1 / 82-S1 | Sanitized acquired `AgentJobRequestMessage` retaining UUID/context names, with a real shell probe that prints the three workflow commands; exact `completejob.stepResults[].annotations` names, 3/2/1 levels, title/range/step number and masked disposable secret. | `crates/listener/src/tests/annotation_reporting.rs` production `Runner::execute_job` → `StepCollector` → `CompleteJobRequest` → `wire::net::complete_job` test. A local HTTP receiver records the actual POST body and verifies its fields. `cargo nextest run -p listener -E 'test(annotation_reporting)'`. |
| AC-2 / 82-S2 | Same shell probe prints no-file, end-only, invalid/descending/multi-line range and `%0A` message commands; compare each resulting JSON object with pinned upstream behavior, including omitted defaults and no secret text. | `crates/execution/tests/command_annotation_probe.sh` through `crates/execution/tests/command_annotation_test.rs` (`cargo nextest run -p execution --test command_annotation_test`), then the listener `annotation_reporting` POST test. |
| AC-3 / 82-S1/S2 | Local composite action prints two annotations and parent script prints one; captured parent step id and `step_number` appear on both nested records; outage job annotation remains top-level. | `annotation_reporting` collector/completion test (`cargo nextest run -p listener -E 'test(annotation_reporting)'`) and existing `watchdog_trip` regression updated for new schema. |
| AC-4 / 82-S1/S3 | `.github/workflows/annotation-82-live.yml` prints the same commands in dedicated toolu and pinned official-runner lanes on the same workflow SHA and host capability; the GitHub Checks annotations have severity/title/path/range matching the local POST oracle. The Checks API omits step association, so the UI must be inspected separately. Run links, runner binary versions, workflow SHA, Checks API JSON and a UI link are recorded. GitHub.com macOS/Linux and supported GHES versions are separate rows. | The feature-branch push triggers the paired workflow: GitHub requires a `workflow_dispatch` file on the default branch, which this branch cannot provide before merge. With `GH_TOKEN`, both required runner names and expected versions set, run `cargo nextest run -p toolu-runner --features live --test annotation_live --run-ignored only -E 'test(annotation_matches_reference)'`. `TOOLU_ANNOTATION_GHES_URL=… TOOLU_ANNOTATION_GHES_TOKEN=…` plus GHES repo/branch and runner/version variables select the GHES test. Missing run, server, token or pinned runner fails the explicit lane; ordinary test compilation does not count it. |
| AC-5 / 82-S3 | No gate warnings/failures; docs list local and remote evidence separately. | `./tools/check.sh all`; inspect `docs/test-coverage.md` and `README.md`. |

## Documentation impact

Update `README.md`'s reporting description and `docs/test-coverage.md` with each #82 scenario, exact checks, platform/backend applicability, and evidence status. Add a dedicated live workflow; document any unavailable live lane explicitly. Keep this design and its plan in `docs/toolu/` as the repo workflow requires, force-add them because that directory is normally ignored.

## Open Questions

None blocking. GHES credentials/host and equivalent official-runner host availability are evidence prerequisites, not design decisions; unavailable lanes remain unverified and must not be claimed as accepted.
