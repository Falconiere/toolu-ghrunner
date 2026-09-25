# Broker control messages — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** Issue #77 broker poll recovery and token refresh

## Problem

The V2 poll deserializes `messageType` into a closed enum. An unrecognized string rejects the entire HTTP 200 response before the listener can retain its `messageId`; the next poll can encounter the same message again. `ForceTokenRefresh` also needs to replace the session OAuth token while the listener stays online.

## Non-Goals

1. Implement toolu self-update or a runner version policy (#78).
2. Implement server-driven configuration updates for `RunnerRefreshConfig`; this message remains explicitly identified and warned about, with its unsupported behavior documented.
3. Change job-only `/acknowledge` semantics, session-conflict handling (#91), or the epic-wide capture/reference harness (#103).

## Architecture

`protocol::messages::MessageType` deserializes known wire strings explicitly and retains any other string in `Unknown(String)`. `ForceTokenRefresh`, `RunnerRefresh`, `AgentRefresh`, `RunnerRefreshConfig`, and `HostedRunnerShutdown` have distinct variants. `listener::message_route` maps each variant to an explicit route. The poll loop advances `lastMessageId` once a complete broker envelope is parsed, including unknown and unsupported control messages. It logs a WARN with numeric ID and a safe type category, never a raw body or untrusted type string. Unknown types and unsupported refresh/config controls continue polling. A hosted shutdown cancels the shared listener token and ends the lifecycle without acquiring a job; the outer always-online loop observes that token and exits instead of reminting.

V2 `DeleteMessageAsync` is a no-op in the pinned official broker listener; its job-only `/acknowledge` call requires `runnerRequestId`. Therefore control-message acknowledgement means advancing the `lastMessageId` cursor on the next poll. Unknown bodies must not be fabricated into job acknowledgements. Existing job acknowledgement remains a single best-effort call after acquisition.

The idle poll loop and mid-job cancellation watcher are mutually exclusive in one listener lifecycle. The idle loop can replace `SessionCtx.token` after a successful exchange; the watcher uses its own token snapshot for later watcher polls while the current job finishes, then hands its latest successful replacement back to `SessionCtx` for session cleanup. `ForceTokenRefresh` signs a new assertion with the JIT RSA parameters and exchanges it through `wire::net::exchange_token`. Subsequent idle poll, acquire, job acknowledgement, and session deletion use the replaced token. Refresh uses a cancellation-aware, bounded retry for transport/429/5xx; 401/403 and invalid responses fail without retry. The current token remains available if refresh fails. The watcher continues its job and backs off after refresh failure; the idle listener returns the classified failure so the outer loop can recover or remint.

## Interfaces / Schema

- `MessageType::{RunnerJobRequest, BrokerMigration, JobCancellation, ForceTokenRefresh, RunnerRefresh, AgentRefresh, RunnerRefreshConfig, HostedRunnerShutdown, Unknown(String)}` accepts JSON `messageType` strings with exact wire spelling.
- `BrokerMessage` retains `messageId`, `body`, and `iv` for every variant. Missing or mistyped envelope fields still fail parsing.
- `MessageRoute` distinguishes acquire, migrate, cancel, refresh token, known unsupported control, hosted shutdown, and unknown skip.
- `SessionCtx` owns the idle token plus JIT credential material needed for re-exchange; the watcher owns a short-lived token snapshot. No credential is emitted in logs.
- `poll_message` remains the production HTTP parser; no alternative parser is introduced for tests.

## Failure modes and edge cases

- An unknown type followed by a valid job advances the cursor and reaches the job. Repeated delivery of the same or an older ID is skipped locally so it cannot execute twice, even if the broker ignores the cursor. A higher ID advances monotonically; a malformed HTTP envelope has no trustworthy ID and follows existing poll backoff.
- A control body may be opaque or encrypted. Unknown and body-free control routes are classified before body decryption; malformed job/migration/cancellation body/decryption follows the existing skip-with-cursor path.
- Unknown type strings may contain arbitrary text. Logs include only an enum category, message ID, and bounded metadata, never the raw type/body/token.
- A job `/acknowledge` failure remains WARN-only and cannot block the completion report. It never causes duplicate execution in the same listener session.
- Refresh cancellation exits promptly and never publishes a partial token. Transient refresh failure retries within a fixed budget; terminal auth/parse errors retain their classification. Mid-job failure does not cancel the job.
- `RunnerRefresh` and `AgentRefresh` are recognized and warned as unsupported self-update requests, following the no-self-update policy selected for #78. `RunnerRefreshConfig` is recognized and warned as unsupported config update. Hosted shutdown exits the listener and must not silently restart in the default run loop.

## Acceptance criteria

- **AC-1:** A captured-shape broker HTTP 200 with an unrecognized `messageType` deserializes with its original ID/body/IV, emits a safe WARN, advances the next poll cursor, and allows a following valid job request to reach acquisition.
- **AC-2:** Redelivery and a failed job `/acknowledge` do not create a tight poll loop or a second job execution; control messages use cursor acknowledgement and job requests retain the existing job-only call.
- **AC-3:** `ForceTokenRefresh` obtains a new OAuth token from the JIT credentials; the next broker request uses that token, and cancellation, transient failure, terminal auth failure, and malformed success responses have bounded, classified behavior without logging secrets.
- **AC-4:** Known refresh/config/shutdown control types each take an explicit route; update/config policy is documented, and `Unknown` is reserved for future names.
- **AC-5:** The full repository gate passes, user documentation and `docs/test-coverage.md` state the verified lane and identify unavailable live/reference/GHES evidence as unverified.

## Acceptance evidence

| AC / scenario | Input and expected observation | Check and applicability |
| --- | --- | --- |
| AC-1 / 77-S1, local | The committed V2 broker envelope, with only `messageType` changed to a future name, retains its ID/body/IV through the production poll parser and reaches the unknown route. | `crates/protocol/tests/message_types.rs`, `crates/wire/tests/broker_poll_parser.rs`, `crates/listener/src/tests/broker_control.rs`. This is a capture-derived variant, not an actual unknown GitHub capture; HTTP cursor behavior and following live job acquisition remain unverified. |
| AC-2 / 77-S2 | The cursor rule rejects a repeated or older envelope ID. Job-only acknowledgement remains on the existing acquire path. | `crates/listener/src/tests/broker_control.rs` and existing `early_ack` coverage. Actual GitHub redelivery and acknowledgement failure behavior for this change remain unverified. |
| AC-3 / 77-S3 | Production code signs with JIT credentials, bounds transient exchange attempts, and publishes a nonempty replacement token only after success. | `cargo check -p listener --lib --tests` verifies integration at compile time. No real `ForceTokenRefresh` capture or service injection is available; token rotation, retry, and log behavior against GitHub.com/GHES remain unverified. |
| AC-4 / 77-S4 | Wire envelopes for every named control type and one future name yield distinct routes; `RunnerRefresh`/`AgentRefresh` warn and skip, config warns and skips, hosted shutdown exits. | `crates/protocol/tests/message_types.rs`, `crates/listener/src/tests/broker_control.rs`; corresponding `cargo test` commands. Hosted shutdown is only applicable when GitHub sends it. |
| AC-5 | Final source tree formats, lints, passes guardrails/tests; docs describe exact verified and unverified scope. | `./tools/check.sh all` in CI and review of `docs/test-coverage.md` plus `docs/architecture.md`. GitHub.com/GHES and pinned official-runner comparison require live lane evidence. |

## Documentation impact

Update `docs/architecture.md` for the broker control-message and token lifecycle and `docs/test-coverage.md` for issue #77 evidence, platform/backend applicability, and unverified live/reference cases. No CLI or configuration key changes.

## Open Questions

No design question blocks implementation. #78 owns a future self-update/version policy; until it changes, `RunnerRefresh` and `AgentRefresh` use the explicit warn-and-skip route. #103 owns the shared capture/reference infrastructure. An actual unknown-message capture, GitHub control injection, and pinned reference-runner comparison are required for a full live-parity claim; none is present in this worktree. They remain visibly unverified and cannot be used as passing evidence at issue closure.
