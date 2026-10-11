# Brainstorm — CompleteJob / StepResult payload parity (#88)

Path: **Full** (public wire contract, secret handling, cross-crate).

## Capsule

- **Outcome:** `completejob` carries the evaluated `environmentUrl` (GitHub
  shows "View deployment"), echoes `billingOwnerId`, and sends `StepResult`s in
  the official runner's wire shape with `action_name` / `ref` / `type` and
  per-step annotations. A secret-bearing URL is dropped with the upstream
  warning. A "Complete job" step hosts the end-of-job evaluation, as upstream.
- **Material defaults / non-goals:** no step debugger, so
  `infrastructureFailureCategory` is modelled but never set; GHES V1
  `JobEvent` completion is not wired in toolu and stays out; pre-step ordering
  (#100/#101 domain) is not changed.
- **Repository evidence:** `#70` job outputs (`job_runner/outputs.rs`,
  `job_spec::evaluate_acquired_outputs`) is the end-of-job evaluation + secret
  skip precedent; `#82` added `StepResult.annotations` and the recording-POST
  test style (`listener/src/tests/annotation_reporting.rs`); `#99` committed
  live captures plus a toolu-vs-hosted-reference workflow lane
  (`step-attrs-99.yml`) and explicitly deferred the missing "Complete job"
  row to #88 (`docs/test-coverage.md`).
- **Risk:** the `StepResult` key rename is unverifiable against GitHub's
  server except by its acceptance (HTTP 2xx) and unchanged UI; the live lane
  needs a locally registered toolu runner.
- **Handoff:** spec.

## Upstream evidence (actions/runner cab9d1c)

- `CompleteJobRequest.cs`: `environmentUrl`, `billingOwnerId`,
  `infrastructureFailureCategory` (all `EmitDefaultValue=false`).
- `StepResult.cs`: **snake_case** members `external_id`, `number`, `name`,
  `action_name`, `ref`, `type`, `status`, `conclusion`, `started_at`,
  `completed_at`, `completed_log_url`, `completed_log_lines`, `annotations`;
  no `outcome`.
- `VssJsonMediaTypeFormatter`: `CamelCasePropertyNamesContractResolver`
  (explicit snake names survive) + `StringEnumConverter(CamelCase)` → status
  `"completed"`, conclusion `"succeeded"|"failed"|"canceled"|"skipped"`.
- `ExecutionContext.Complete`: Task records add a `StepResult` with
  `StepTelemetry.{Action,Ref,Type}` and the record's issues as annotations.
- `Handler.PopulateActionTelemetry` + handler overrides: run → `run` + shell
  command; repository → `name[/path]` + `ref` (self → `path`, no ref), then
  `node20|node24` / `composite` / `Dockerfile`; registry → `DockerHub` +
  image; setup/complete → `runner` + `setup_job` / `complete_job`.
- `JobExtension.FinalizeJob` (L829-855): evaluate `ActionsEnvironment.Url`
  against `string-runner-context-no-secrets`; masked value differs → warning
  `Skip setting environment url as environment '<name>' may contain secret.`;
  evaluation error → `Failed to evaluate environment url` + job Failed.
  `JobRunner` sends it only when the token is a string.
- `infrastructureFailureCategory`: only from a categorized infrastructure
  issue; sole category is `debugger_tunnel_failure`.

## Decisions (Jev-informed)

| Decision | Choice | Rejected | Jev |
| --- | --- | --- | --- |
| StepResult wire shape | upstream snake_case + string enums, drop `outcome` | keep camelCase + numeric | upstream 1.00 |
| infrastructureFailureCategory | field exists, never set; documented N/A | invent toolu categories | absent 1.00 |
| Literal URL containing a secret | suppress + warning (safe deviation; AC 88-S2) | send verbatim like upstream | suppress 0.54 (uncertain; AC wording decides) |
| Metadata transport | new `RunnerEvent::StepMetadata` emitted when the handler kind is known | new fields on every `StepCompleted` constructor | agent judgment |
| Where the evaluation lives | engine "Complete job" step (also hosts #70 output warnings) | job-level annotation keyed by job id (dropped today) | agent judgment |
