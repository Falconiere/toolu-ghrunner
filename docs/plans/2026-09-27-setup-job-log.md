# Set up job diagnostics — Plan

**Date:** 2026-09-27   **Status:** Approved   **Spec:** docs/specs/2026-09-27-setup-job-log-design.md   **Topic:** #87

## Evidence and approach
Reuse listener event fan-out, StepCollector and StepReportQueue. Metadata comes
from JIT/acquired job/host; action resolution already exposes validated SHA.
Maintain execution scheduling and document deferred diagnostics. Use captured
incoming_contexts_matrix_0.json and actual shell/public action data; do not claim
private service or UI verification from a recording server.

## Workstream summary
Setup lifecycle and metadata → action diagnostics → documentation and full gate.

## Steps (machine-readable)
```json
[
  {"id":"setup-diagnostics","title":"Implement metadata, setup lifecycle, action diagnostics and synchronized evidence documentation","ac_refs":["AC-1","AC-2","AC-3","AC-4"],"input":"Captured GitHub.com acquired message, captured public action REST response and archive, real shell, filesystem failure and cancellation","check":"./tools/check.sh all && git diff --check","paths":["**"]}
]
```

## Critical files
crates/listener/src/{setup_step,execution_loop,handler,step_reporter}.rs;
crates/listener/src/tests/{setup_step,setup_forwarding}.rs;
crates/shared/src/events.rs;
crates/execution/src/execution/actions/prefetch.rs and sibling tests;
crates/execution/src/execution/job_runner/prepared.rs;
README.md; docs/test-coverage.md; affected module README tables.

## Verification
Test absent/malformed metadata, redaction, early setup failure/cancel, live channel
and collected completion results; action ref/subpath/SHA and duplicate cold/warm
behavior. Full gate is mandatory. Inspect final diff, verify every ledger check,
commit scoped changes, fetch/rebase and rerun gate if main moved, push authorized
feature branch, create conventional PR closing #87 and linking #67. Verify Linux
and macOS GitHub.com CI; explicitly mark GHES/private/live/reference lanes skipped
and unverified. Run toolu-review, ledger --verify and verdict checks before push.
Use authorized pr-babysit handoff, report phases and ready; never merge.

## Plan review
Approved after deterministic AC-reference and dependency inspection. All four ACs
map to checks and docs are explicit. Jev alignment 0.62 prompted checking evidence
scope: private/GHES/live lanes are explicitly unverified, and network resolution
coverage must not be described as passing from a metadata-only test.

## Deviations
Orchestrator clarified that bare cargo test is deliberately denied. All checks
use the approved full gate; no hook bypass.

The approved verification route is one whole-workspace gate. The ledger therefore
uses one integrated step covering all ACs instead of three identical full-gate
checks. Production replay now includes real REST/codeload capture replay through
ensure_action and Runner prefetch, strengthening the initially planned archive
component check without claiming private/live verification.
