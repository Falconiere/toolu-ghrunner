# Workflow and job environment — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** #69

## Problem

Acquired `environmentVariables` mappings are ignored. Workflow/job env therefore
vanishes from processes and expressions. Script expressions also miss explicit
step env overrides that currently reach only the process.

## Non-Goals

1. Change expression context import (#68), process CI flags (#72), composite
   schema restrictions (#102), or file-command restrictions (#96).
2. Claim passing live, Linux, or GHES evidence without executing those lanes.

## Architecture

Match actions/runner at `cab9d1c3901e45c7705889c4f88284fdd93f4ae5`,
`JobExtension.cs:235-250`: evaluate each mapping against one pre-layer context,
then merge the completed layer into job env before evaluating the next layer.
Run this after context import and secret registration, before container evaluation
and service startup. This preserves the existing infallible `build_context` API.
Reuse the expression evaluator and `ExecutionContext::set_env`; do not mutate the
host process environment. Each job owns its context.

Script steps resolve their env once against the pre-step context, then push the
existing temporary env overlay before rendering body, shell, and working directory.
Pop it on success and error. File-command updates remain in persistent job env;
the current process cannot see its own future `GITHUB_ENV` writes. Existing action
overlays and composite scope machinery remain authoritative for nested actions.

## Interfaces / Schema

- `AgentJobRequestMessage.environment_variables: Vec<serde_json::Value>` preserves
  raw JSON; the wire name and order stay unchanged and absent means an empty list.
- Internal `apply_job_environment(&[serde_json::Value], &mut ExecutionContext) ->
  Result<(), RunnerError>` decodes each layer into `TemplateToken` during setup,
  validates mapping/key/scalar shapes, evaluates expressions
  once, and merges a whole layer only after it succeeds.
- Literal strings remain literal, including expression-looking text returned by
  an expression. Expression scalar results use GitHub string coercion; mapping or
  sequence env values fail. Empty mappings and null layers are no-ops. Upstream serialization omits empty
  `map`/`lit`, false `bool`, and zero `num` payloads; those omitted fields retain
  their default values.
- Keys are case-sensitive on supported Linux/macOS. Literal and scalar expression
  keys are evaluated against the same pre-layer snapshot as values.

## Failure modes and edge cases

Absent/empty env is unchanged. Empty strings, multiline strings, booleans, numbers,
and null scalar values preserve their string coercion. Later layers override earlier
ones; entries in one layer cannot observe each other's assignments. Missing allowed
properties become empty. Malformed wire payloads, invalid expressions or non-mapping layers fail job setup
before any process starts. Errors identify the layer without embedding secret
values. Secret registration precedes evaluation; logs retain existing sink masking.
Step overrides are discarded after the step, including errors, while file commands
persist for later steps. Consecutive jobs cannot share these values.

## Acceptance criteria

- **AC-1:** Real shell execution resolves ordered workflow/job mappings over
  github/secrets/vars/inputs/matrix/needs/strategy and the previous env layer;
  step process and expression values show step > job > workflow (69-S1/S2).
- **AC-2:** Real script, Node, and nested composite actions receive exact empty,
  multiline and secret-bearing strings; durable sink tests show redaction (69-S2).
- **AC-3:** A writer sees its original env; later steps see its GITHUB_ENV update;
  explicit overrides last one step, and successive jobs stay isolated (69-S3/S4).
- **AC-4:** Empty/absent inputs remain compatible; malformed layers fail before
  execution without revealing secret values; same-layer references use the old snapshot.
- **AC-5:** Full gate passes and user docs plus coverage map distinguish local replay,
  live pinned-reference comparison, platform/backend applicability and unverified lanes.

## Acceptance evidence

| AC | Real input, expected result, and check |
| --- | --- |
| 1 | Replay the sanitized #68 acquisition envelope with explicitly documented env/step transformations and committed real Bash assertions: workflow `shared=workflow`, job `shared=job`, job prior=`workflow`, step=`step`; all seven context values match acquisition. `cargo test -p execution job_environment` exercises the production Runner path from a sibling tests module. |
| 2 | Same acquisition replay with checked-in Node and nested composite actions; exact empty/multiline bytes and secret probe are observed in processes. Existing `secret_sink_coverage_test` plus job-env replay verify masker registration and durable journal/listener sinks; no mocked backend success. |
| 3 | Real file writes: writer retains job value, next overridden step sees override, following step sees file value; nested action writes persist, local overrides do not. Second job omits the first job's key and observes absence. Same runnable replay command. |
| 4 | Boundary transformations of acquisition replace/remove layers and inject invalid tokens. Production Runner reports Failure with no StepStarted and no secret-bearing diagnostic. Same replay command, plus direct sibling tests for atomic layer snapshot behavior. |
| 5 | `./tools/check.sh all`; docs map every AC and 69-S1–S4. A committed workflow/action probe is the input for identical toolu/reference runs pinned to the epic baseline. GitHub.com log inspection must prove masking; missing online runners currently leave this unverified. Linux host replay is applicable in CI; macOS host replay locally; GHES uses the same env wire path but requires separate server evidence. Docker-specific setup is owned by #73/#75. |

Transformed acquisitions are labelled as transformations, never new server captures.
A new sanitized acquisition with nonempty env layers is required for live wiring
sign-off. No missing lane counts as passing acceptance.

## Documentation impact

Update README.md, docs/architecture.md, docs/test-coverage.md and execution module
README rows. Durable design lives in tracked docs/specs, matching recent epic work;
the executable plan lives in ignored docs/toolu/plans per the workflow convention.

## Open Questions

No unresolved implementation choice. Live runner availability is an external
verification prerequisite; report needs-human if it cannot be provisioned within
the authorized worktree/branch scope, after completing independent local work.
