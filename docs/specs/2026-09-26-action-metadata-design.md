# Action metadata — Design

**Date:** 2026-09-26   **Status:** Approved   **Author:** Codex   **Topic:** #85 / epic #67

## Problem

Run steps lack action identity; repository/ref and runner environment are absent.
The acquired step's wire `name` is discarded, and action handlers currently use
contextName/UUID for GITHUB_ACTION instead of the upstream action name.

## Non-Goals

1. Change output scope, timeline UUIDs, action resolution, or stage scheduling.
2. Reimplement GitHub's server-side generated-name allocator.
3. Claim live or platform parity from local component coverage.

## Architecture

Preserve optional ActionStep.name. At the top-level step boundary install its
wire name (fallback to nonempty contextName for legacy locally built steps).
Restore the prior github metadata after execution, including errors. Deferred
posts reinstall the originating step name. Embedded composite steps inherit
the enclosing action name, as in pinned upstream CompositeActionHandler.

Each action handler sets repository/ref from a remote repository reference;
local, script and registry-image steps clear them. Nested calls restore parent
values. Render nested with inputs with the child reference, matching ActionRunner;
render nested step env in the enclosing context, matching CompositeActionHandler.
Export runtime-owned metadata last when building child environments,
so user env/file commands cannot leave stale values. Keep action_path's existing
scope behavior. Set runner.environment and RUNNER_ENVIRONMENT to self-hosted
in the existing runner-context initialization and process environment assembly.

Pinned source: actions/runner cab9d1c3901e45c7705889c4f88284fdd93f4ae5,
StepsRunner.cs:118 (`Action.Name`), ActionRunner.cs:143 (remote repo/ref or null),
CompositeActionHandler.cs (parent github copy, no action-name reassignment).
Captured incoming_contexts_matrix_0.json includes __run, __run_2,
__actions_checkout and __self; UUIDs remain reporting IDs.

## Interfaces / Schema

Add serde-default optional `name: Option<String>` to shared ActionStep.
Use an internal metadata snapshot/restore helper in execution; no public config.
Expose github.action/action_repository/action_ref consistently with child env.
Absent repository/ref are empty strings in expressions and process env, preventing
an inherited host or prior-step value from leaking. Missing name/contextName
exports empty rather than a UUID. Remote subpaths do not enter repository name.

## Failure modes and edge cases

Skipped steps do not leak metadata. Resolution/render/launch errors restore parent
metadata. Repeated remote actions retain their separate wire names and refs;
post stages reuse the original action name, not their generated report UUID.
Resolved-action probe tests cover remote reference dispatch without pretending
to acquire or download a remote action; those service lanes stay unverified.
Nested composites restore parents across multiple invocations; embedded run
steps clear repository/ref while retaining top-level action identity.
Job contexts own their state, so concurrent jobs cannot share metadata.
Linux containers receive the same strings as host steps. macOS Docker remains
unsupported. GitHub.com and GHES use the same acquired-message contract.

## Acceptance criteria

- **AC-1:** Captured named/unnamed run steps and repeated remote/local/registry
  actions expose upstream name and correct repo/ref, without stale values (85-S1).
- **AC-2:** Real Node pre/main/post and repeated nested composites expose scoped
  metadata and restore parent values, including after failures (85-S2).
- **AC-3:** Real process env and expressions both report self-hosted on Linux
  and macOS; supported Linux container steps receive consistent metadata (85-S3).
- **AC-4:** Full repository gate passes, docs and evidence map cover all scenarios,
  and GitHub.com Linux/macOS CI verifies the real-process replay.

## Acceptance evidence

Use the sanitized GitHub.com incoming_contexts_matrix_0.json acquisition in
crates/execution/tests, preserving UUID/name/contextName and token structure.
Replay through Runner::execute_job with real shell/Node probes and local actions;
document substitutions to action paths/scripts. Add sibling tests in
crates/execution/src/execution/tests/action_metadata.rs, run with
`cargo test -p execution action_metadata`. Assert emitted probe values and job
completion, not a manually constructed context alone. Include unnamed/named,
repeated, absent metadata, nested restoration, stage and error cases.

Use existing GitHub.com CI for Linux/macOS workspace tests and its real Linux
Docker lane. Record source revisions, commands and CI links in the coverage map
and PR. Per the orchestrator decision on 2026-09-26, GHES/live acquisition and
official-runner comparison lanes remain explicitly unverified/skipped because
credentials are unavailable; they do not block this delivery.
`./tools/check.sh all` is mandatory before delivery.

## Documentation impact

Update README environment behavior and docs/test-coverage.md with AC/scenario
mapping and actual lane results. Add the metadata module to execution's README.
Durable design/plan use tracked docs/specs and docs/plans, following #81/#102;
docs/toolu is gitignored.

## Open Questions

None. The orchestrator explicitly resolved unavailable backend/reference lanes
as non-blocking, unverified evidence; Linux/macOS GitHub.com CI remains required.

## Spec review

Approved after checking upstream naming, metadata restoration, all 85-S1–S3
scenarios, existing production replay and Linux/macOS CI paths. The orchestrator
explicitly narrowed unavailable GHES/live/reference evidence; no passing claim
will be made for those lanes. Jev scope-alignment judgment: 0.81.

Read-only upstream review correction: absent repo/ref use StringContextData and
therefore empty strings, not expression nulls. Implementation and tests agree.
