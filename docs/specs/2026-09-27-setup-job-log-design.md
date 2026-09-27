# Set up job diagnostics — Design

**Date:** 2026-09-27   **Status:** Approved   **Author:** Codex   **Topic:** #87 / #67

## Problem
The listener reports successful setup with two fixed lines before execution.
Operators cannot inspect identity, permissions or downloaded action revisions.

## Non-Goals
1. Change action resolution, background prefetch, retry or execution ordering.
2. Implement hosted image provisioning or fabricate missing metadata.
3. Claim unavailable GHES/private/live UI/reference-runner verification.

## Architecture
Reuse StepStarted/Log/StepCompleted, the report queue, collector, masked fan-out,
and streaming uploader. Start setup before engine events. Complete setup at the
first real step start (Cancelled if the job token is already cancelled); if the
job terminates first, use its conclusion. Keep the
setup uploader until event-stream closure so lazy/nested downloads append to the
same log. This preserves current background preparation semantics; setup timing
covers initial preparation, while its log also includes deferred downloads.
Route all initial-preparation logs, including named hook output, into setup. A unique setup UUID is
listener-owned; execution emits logs with a shared reserved setup routing ID,
which the forwarder translates. No new serialized event variants.

Metadata reads only allowlisted fields: actual package version and separate
protocol compatibility version, host OS/arch and hostname, JIT agent name,
message system.runnerGroupName, system.github.token.permissions (JSON object of
strings), github.secret_source. Missing identity fields say unavailable; optional
permissions/secret source are omitted. Permissions use ##[group]/##[endgroup].
Malformed permissions produce a fixed diagnostic without reflecting input.
Register message secrets before generating or forwarding metadata.

ActionFetcher emits intended owner/repository/subpath@ref and validated immutable
SHA after successful ensure, including cache hits. Deduplicate identical ref/SHA
pairs per job; different subpaths remain distinct. Never include URL, response
body or download credential. Failed preparation remains visible through existing
errors; do not invent a successful download. Existing masking applies at sinks.
Pinned upstream: actions/runner cab9d1c3901e45c7705889c4f88284fdd93f4ae5,
Runner.Worker/JobExtension.cs (identity, permissions and secret-source output).

## Interfaces / Schema
SessionCtx gains optional runner_name from JIT. FwdConfig and ForwarderState own setup state
(UUID, preamble, cancellation token, completion latch). shared::events defines the reserved setup log
routing ID. ActionFetcher gains an optional event sender via a builder so existing
callers remain compatible. Production prepared::execute installs that sender.
No config or wire schema change. New helpers stay below 500 code lines.

## Failure modes and edge cases
No results endpoint still yields journal and live output. Reporting errors remain
best effort. Setup failure/cancellation preserves earlier masked lines and records
failure/cancel instead of success. Empty jobs complete setup from job conclusion.
Missing/empty identity is unavailable; malformed permissions never fail the job.
Cold/warm/nested/duplicate/subpath actions log resolved identities; failed resolution
has no invented SHA. Log upload remains open through deferred fetch events.

## Acceptance criteria
- **AC-1:** Captured job replay reports actual version/OS/arch/name/group/machine,
  grouped permissions when present and secret source when provided (87-S1/S3).
- **AC-2:** Successful cold/cache action fetches report intended ref including
  subpath and immutable SHA, without credentials; duplicates are bounded (87-S2).
- **AC-3:** Production replay preserves masked partial diagnostics through setup
  failure/cancellation, live forwarding and combined logs (87-S3).
- **AC-4:** Full gate and GitHub.com Linux/macOS CI pass; documentation maps all
  scenarios and explicitly identifies unavailable verification lanes.

## Acceptance evidence
AC-1/3: sibling listener tests replay sanitized incoming_contexts_matrix_0.json
through Runner and production forwarder. Preserve captured wire types and IDs;
replace steps with real shell probes, remove remote checkout, vary metadata only
for absent/malformed boundaries. Inspect setup events/results and actual live
channel output, including early filesystem failure and cancellation. Command:
`./tools/check.sh all`.
AC-2: execution sibling tests use existing real public action download test inputs
and recorded GitHub resolver payloads; assert cold/warm ref/SHA emitted by fetcher
and production sender wiring. `./tools/check.sh all` plus existing action
download suite. Private scoped-service acquisition remains unverified; do not use
mock success to claim it. AC-4: `./tools/check.sh all` and Linux/macOS CI links.
Unavailable GHES/live/private/reference lanes are skipped/unverified, not blockers,
per orchestrator policy. No universal parity claim.

## Documentation impact
Update README setup-log behavior and docs/test-coverage.md scenario/evidence map.
Track durable design and plan in docs/specs and docs/plans (docs/toolu is ignored).

## Open Questions
None. Preserve lazy action scheduling and document late log append explicitly.

## Spec review
Approved: source fields and sink reuse verified against acquired-message and
reporting code; Jev alignment 0.80. Deferred action-log timing is explicit.
Private/GHES/live lanes remain unverified under the orchestrator policy.

Implementation evidence refinement: static endpoints replay an actual public REST
revision response and unmodified codeload archive. Production ensure_action and
Runner prefetch are exercised; no successful service payload is invented.
