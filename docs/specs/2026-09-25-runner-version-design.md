# Runner identity and operator-managed updates — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** Issue #78

## Problem

Session/poll advertise an invented `3.0.0`, while acknowledgement advertises
toolu's Cargo product version. GitHub applies runner-version eligibility rules;
neither the mismatch nor an invented higher number is a supported strategy.

## Non-Goals

1. Implement a self-updater or execute packages named in broker messages.
2. Claim complete official-runner parity, indefinite queue eligibility, or GHES
   compatibility without a tested server/version.
3. Change token refresh (#77), acquisition OS spelling, registration or daemon
   lifecycle (#92/#95), or GitHub's service policy.

## Architecture

A protocol-owned `runner_version::COMPATIBILITY_VERSION` is the single source
for session agent.version and poll/ack runnerVersion. Pin `2.337.0` to official release commit
`397b032cbf865e9c3ddfab89d533ec19325e1273`; the epic's later source-inspection
snapshot `cab9d1c3901e45c7705889c4f88284fdd93f4ae5` retains that version.
This identity is independent of toolu's release number. Remove caller-selected `PollParams.runner_version` to prevent drift
between idle polls, busy cancellation polls and acknowledgement. The existing
CLI `--version` remains the product version; listener startup logs both identities.
This is a declared compatibility target, not proof of all features or service
eligibility. Raising the number alone is not an update procedure.

Retain #77's warn-and-skip route for RunnerRefresh/AgentRefresh. Advance the
cursor before processing the control; repeated IDs remain suppressed. Both
idle and busy paths continue without cancellation, download, install or exec.
Warn with fixed guidance to install a validated toolu release. Keep bodies opaque
and never print body/URL/token data. RunnerRefreshConfig remains unsupported;
ForceTokenRefresh retains its separate OAuth behavior.

## Interfaces / Schema

- New public protocol constant `runner_version::COMPATIBILITY_VERSION: &str`.
- Remove `PollParams.runner_version`; HTTP field names/types stay unchanged.
- Session JSON `agent.version`, GET /message query `runnerVersion`, and POST
  /acknowledge query `runnerVersion` all contain the constant.
- Keep `disableUpdate=true`. No environment override, updater or new CLI option.
- Listener startup INFO: `product_version`, `compatibility_version`, update policy.

## Failure modes and edge cases

Transport/status failures retain current error propagation; a consistent identity
does not convert errors into success. Poll cursor 0, positive and repeated IDs
retain their behavior. Refresh bodies may be absent/opaque/invalid as update
payloads: no parsing is needed to ignore them safely. Refresh does not fail or
cancel an active job. A later job still reaches acquisition. Required upstream
updates can leave jobs queued even with an online runner; stop scheduling affected
instances, deploy a validated toolu release, restart/remint, and revalidate a real
job. No local 30-day simulation establishes service behavior.

## Acceptance criteria

- **AC-1:** Requests emitted by one build carry `2.337.0` in all three protocol
  surfaces; product version is independently observable in CLI/startup logs.
- **AC-2:** RunnerRefresh/AgentRefresh in idle and busy production handlers warn,
  preserve cancellation state, advance the cursor and allow the following job;
  repeated IDs do not rerun control handling, and opaque bodies are never logged.
- **AC-3:** Operator docs name the baseline, maintenance owner, 30-day and critical
  update policy, deployment/revalidation and incident recovery steps.
- **AC-4:** A real GitHub.com register/session/poll/acquire/complete run and pinned
  official comparison record runner, workflow and host revisions and outcome.
  Missing live lanes remain unverified and block a full acceptance claim.
- **AC-5:** The unchanged `./tools/check.sh all` passes and the committed coverage
  map lists each original criterion and 78-S1 through 78-S4 with evidence limits.

## Acceptance evidence

| AC / scenario | Real input, expected output, boundaries | Runnable check |
| --- | --- | --- |
| AC-1 / 78-S1 | Real reqwest requests emitted by the production session/poll/ack transports, captured by a loopback TCP recorder that closes without inventing successful GitHub replies. Exact JSON/query identity, disableUpdate, first/subsequent cursors; disconnect propagates errors. This tests outgoing wire behavior, not eligibility. | `cargo test -p wire --test runner_version`; existing poll/CLI tests |
| AC-2 / 78-S3 | Labelled refresh variants of committed broker envelope through actual idle/busy handlers; safe warning, unchanged job token, monotonic cursor and next-job routing. These are replay variants, not live refresh captures. | `cargo test -p listener --lib runner_update_policy` |
| AC-3 / 78-S4 | Official policy and pinned baseline; operator review checks owners, update/revalidation and incident procedure. | documentation assertions in plan plus `git diff --check` |
| AC-4 / 78-S2 | Real isolated JIT runner and official 2.337.0 run of the same workflow revision. Record links and completion, session/poll/ack evidence without credentials. | live lane run commands and evidence to be recorded in `docs/test-coverage.md`; no mocked service substitutes |
| AC-5 / all | Final workspace source and scenario map. | `./tools/check.sh all` |

Local wire and handler checks apply on Linux/macOS. GitHub.com live evidence is
separate. GHES has no endpoint/credentials in this epic environment and remains
unverified; no new GHES eligibility claim is made. Critical-update and 30-day
behavior are documented policy, not experimentally simulated claims.

## Documentation impact

Update README, `docs/architecture.md`, `docs/test-coverage.md`, and add
`docs/runner-updates.md`; update AGENTS.md's module map for the identity source.
Durable spec/plan use tracked `docs/specs` and `docs/plans`, following recent
merged epic children; local ledger discovery copies live under ignored docs/toolu.

## Open Questions

Design choices are resolved above (Jev supported the pinned baseline/manual
policy against the issue and code evidence). Live GitHub eligibility must be
observed before claiming AC-4. No GHES credentials exist per the epic's recorded
operator statement; mark that lane unverified. If available GitHub credentials or
host execution cannot run the required live lane, report needs-human with the
specific infrastructure gap rather than marking it passed.

Spec review: no remaining findings; interfaces, failure behavior, AC evidence and
service-owned acceptance limits checked against issue #78 and merged #77.
