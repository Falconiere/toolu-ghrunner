# Composite working directory — Design

**Date:** 2026-09-26   **Status:** Approved   **Author:** Codex   **Topic:** #81, epic #67

## Problem

Composite manifests discard `working-directory`, and every inner script runs at
workspace root. Commands therefore write or read the wrong files.

## Non-Goals

1. Change shell selection (#80), job defaults (#71), or expression policies (#102).
2. Change action resolution, process-wide cwd, or nested invocation workspace.
3. Claim live/reference/GHES parity from local replay alone.

## Architecture

Add an optional string to `CompositeStep` and its raw YAML representation.
Render it immediately before script dispatch with the existing composite step
expression policy and the same live input/env/steps snapshot used for shell/run.
Resolve via `workspace.join(rendered)`; absolute paths replace the base. Pass the
owned path by reference through `execute_composite_script` to host/container
shell parameters. The workspace remains immutable between steps/invocations.

The pinned upstream [ScriptHandler](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/Handlers/ScriptHandler.cs)
combines workspace with the directory and consults job defaults only for an empty
scope. Composite steps must therefore ignore caller defaults. Reuse #102's inner
failure/outcome/cleanup handling rather than adding a second error path.

## Interfaces / Schema

`CompositeStep.working_directory: Option<String>`; YAML `working-directory` maps
to the same optional raw field. No new configuration. Existing composite
struct literals must initialize the new field. Directory applies to `run` only.

## Failure modes and edge cases

Absent, empty, or expression-rendered empty directory uses workspace root.
Relative paths (including `..`) are workspace-relative; absolute paths and spaces
are preserved without shell splitting, trimming, canonicalization, or creation.
Each step resolves independently, including nested composites and subsequent
caller steps. Missing/not-directory paths fail process launch. Invalid syntax or
forbidden expression roots fail before execution; raw outcome is failure and
existing continue-on-error/condition rules determine effective conclusion and
later cleanup. No global cwd mutation means concurrent jobs remain isolated.

## Acceptance criteria

- **AC-1:** Literal/input/env/prior-output relative directories, absolute paths,
  spaces, and absent/empty values produce exact real `pwd` and file locations (81-S1).
- **AC-2:** Nested actions use their own inputs/action paths and explicit cwd;
  absent cwd stays workspace-root despite caller defaults, and subsequent inner
  and caller steps use their own directories (81-S2).
- **AC-3:** Missing directory and malformed directory expression do not execute
  their script, report inner failure, skip ordinary successors, and execute
  eligible `failure()`/`always()` cleanup; continued failure retains raw outcome
  while allowing ordinary successors (81-S3).
- **AC-4:** Full repository gate passes and committed documentation maps all
  scenarios to real input, expected result, commands, and measured platform lanes.

## Acceptance evidence

Use `crates/execution/tests/incoming_contexts_matrix_0.json`, a sanitized actual
GitHub.com acquisition already used by #102. Retain wire IDs/types; substitute
local action path and caller script/default values explicitly in the replay.
Execute via `Runner::execute_job` with committed local Bash actions, never a
manually built ExecutionContext. Colocated real-process tests in
`crates/execution/src/execution/tests/composite_working_directory.rs` cover AC-1–3;
`cargo test -p execution composite_working_directory` runs them. The same actions
are suitable for official-runner comparison; any missing live/reference or GHES
lane stays explicitly unverified in `docs/test-coverage.md`. Host behavior applies
to Linux/macOS and both GitHub.com/GHES; container cwd is passed through the
existing translator and requires its Linux Docker lane for parity evidence.
`./tools/check.sh all` proves AC-4's gate. Record actual results after execution.

## Documentation impact

Update README composite behavior and docs/test-coverage.md with scenario mapping,
provenance, exact commands/results and unavailable lanes. Durable design/plan live
in tracked docs/specs and docs/plans following #102; docs/toolu is gitignored.

## Open Questions

None for implementation. External lane availability is checked during execution;
missing required infrastructure is reported explicitly, never treated as success.

## Review

Spec-review: approved after checking S1–S3, upstream default scope, existing
error-loop behavior and real acquisition provenance. Jev alignment: 0.92.
External lanes are evidence obligations, not implied passing claims.
