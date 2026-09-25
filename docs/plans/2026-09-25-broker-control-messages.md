# Broker control messages — Implementation plan

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/specs/2026-09-25-broker-control-messages-design.md   **Topic:** Issue #77 unknown broker types and token refresh

## Evidence and approach

`protocol::BrokerMessage` currently fails deserialization for any unlisted type, before `job_lifecycle` can preserve `messageId`. The listener uses a cursor for control messages and a separate job-only acknowledgement requiring `runnerRequestId`. The pinned official V2 broker listener's `DeleteMessageAsync` is a no-op, so no synthetic acknowledgement request is justified. `SessionCtx.token` is a `String` read by poll, acquire, acknowledgement, and deletion. The plan adds explicit control routing, token rotation in the exclusive idle/watch phases, bounded exchange errors, and production parser tests on capture-shaped input. The existing V2 broker fixtures are synthetic/capture-shaped; actual unknown-message and reference-runner live evidence remains unverified unless obtained.

## Workstream summary

Pin the wire contract with a failing test, implement parsing and routes, wire refresh into both poll paths, then verify documentation and the full gate before delivery. Tests use the committed broker envelopes and production parser; unavailable live behavior is recorded as unverified.

## Steps (machine-readable)

```json
[
  {
    "id": "wire-types",
    "title": "Parse known and future broker types without losing the envelope",
    "ac_refs": ["AC-1", "AC-4"],
    "paths": ["crates/protocol/src/messages.rs", "crates/protocol/tests/message_types.rs", "crates/toolu-runner/tests/fixtures/broker_message_job_request.json", "crates/toolu-runner/tests/fixtures/broker_message_migration.json", "crates/listener/src/message_route.rs", "crates/toolu-runner/tests/gh_compat_cancel.rs"],
    "input": "Committed V2 broker envelopes with a capture-derived future type plus every named control type, malformed field, and untrusted type text",
    "check": "cargo test -p protocol --test message_types && cargo test -p toolu-runner --test gh_compat_cancel"
  },
  {
    "id": "refresh-transport",
    "title": "Classify and bound OAuth token re-exchange",
    "ac_refs": ["AC-3"],
    "depends_on": ["wire-types"],
    "paths": ["crates/wire/src/net/auth.rs", "crates/wire/src/net/messages.rs", "crates/wire/tests/broker_poll_parser.rs", "crates/listener/src/broker_refresh.rs"],
    "input": "Committed V2 broker envelope through the production HTTP response parser; inspect exchange status classification and bounded refresh code without fabricating broker or OAuth responses",
    "check": "cargo check -p wire --test broker_poll_parser && cargo check -p listener --lib --tests"
  },
  {
    "id": "listener-route",
    "title": "Advance the cursor, skip unknowns, rotate token, and retain one job execution",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4"],
    "depends_on": ["wire-types", "refresh-transport"],
    "paths": ["crates/listener/src/handler.rs", "crates/listener/src/job_lifecycle.rs", "crates/listener/src/broker_message.rs", "crates/listener/src/broker_refresh.rs", "crates/listener/src/message_route.rs", "crates/listener/src/tests/broker_control.rs", "crates/listener/src/tests/early_ack.rs", "crates/listener/src/tests/finalize_split.rs", "crates/listener/src/tests/helpers.rs", "crates/listener/src/tests/startup_overlap.rs", "crates/listener/src/tests/step_report_queue.rs", "crates/listener/src/tests/watchdog_trip.rs", "crates/listener/src/lib.rs", "crates/toolu-runner/tests/fixtures/broker_message_job_request.json"],
    "input": "Capture-derived variants of the committed V2 broker envelope for unknown, redelivery, named control, and valid job routing; inspect production poll and refresh paths",
    "check": "cargo test -p listener --lib broker_control && cargo test -p listener --lib early_ack"
  },
  {
    "id": "docs-gate",
    "title": "Document measured coverage and pass the required gate",
    "ac_refs": ["AC-5"],
    "depends_on": ["listener-route"],
    "paths": ["docs/architecture.md", "docs/test-coverage.md", "docs/specs/2026-09-25-broker-control-messages-design.md", "docs/plans/2026-09-25-broker-control-messages.md", "crates/protocol/src/messages.rs", "crates/protocol/tests/message_types.rs", "crates/wire/src/net/auth.rs", "crates/wire/src/net/messages.rs", "crates/wire/tests/broker_poll_parser.rs", "crates/listener/src/handler.rs", "crates/listener/src/job_lifecycle.rs", "crates/listener/src/broker_message.rs", "crates/listener/src/broker_refresh.rs", "crates/listener/src/message_route.rs", "crates/listener/src/tests/broker_control.rs"],
    "input": "The final branch source, issue #77 scenario map, platform/backend applicability, and explicitly unverified live/reference lanes",
    "check": "./tools/check.sh all && git diff --check"
  }
]
```

## Critical files

Create `crates/protocol/tests/message_types.rs`, `crates/wire/tests/broker_poll_parser.rs`, and `crates/listener/src/tests/broker_control.rs`. Modify `crates/protocol/src/messages.rs`, `crates/wire/src/net/auth.rs`, `crates/wire/src/net/messages.rs`, `crates/listener/src/message_route.rs`, `crates/listener/src/handler.rs`, and `crates/listener/src/job_lifecycle.rs`; extract `broker_message.rs` and `broker_refresh.rs` to keep modules within the repository size limit. Update existing listener test contexts and module declarations. Update `docs/architecture.md` and `docs/test-coverage.md`.

## Verification

The parser red test failed on the closed enum before the implementation. The wire parser red test failed before the shared production parser was added. The focused checks use committed, capture-shaped broker data without a fabricated HTTP or OAuth service. Run `./tools/check.sh all` on the final source. If local test binaries remain stuck at macOS `_dyld_start`, record the failure in the PR and use GitHub Linux `ci` plus `ci-macos` as the gate evidence, as authorized by the issue orchestrator. Before push, commit the scoped change, fetch/rebase if main moved, verify the ledger against the branch diff, and run local review. The PR must cite exact evidence and mark live GitHub/reference/GHES cases unverified until observed.

## Deviations

The issue orchestrator required real-data-only tests and prohibited mocks. The earlier approved plan's local HTTP/OAuth replay was removed. Consequently the live token refresh and redelivery paths have compile-time and code-review evidence, not a passing local service interaction; this limitation must remain explicit at issue closure.
