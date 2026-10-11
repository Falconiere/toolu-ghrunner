# Session-create 409 conflicts — Design

**Date:** 2026-10-11   **Status:** Approved   **Author:** Claude (epic worker, #91)   **Topic:** Classify broker session-create 409, retry it boundedly, then exit with a distinct code

## Problem

`wire::net::create_session` maps every non-2xx to `RunnerError::Protocol`, so a
409 ("another live session exists for this registration") is indistinguishable
from any other failure. The always-online `run` loop then treats it as transient
and backs off 1 s → 60 s forever; `--once` and `boot` exit as a generic failure.
Upstream (`actions/runner` `cab9d1c`) classifies 409 as
`TaskAgentSessionConflictException` (`BrokerHttpClient.CreateSessionAsync`),
retries every 30 s until 4 min of conflicts accumulate
(`MessageListener.IsSessionCreationExceptionRetriable`), then the listener exits
with `ReturnCode.SessionConflict = 5`, which its service wrapper treats as
"stop, no retry".

Live evidence (github.com, 2026-10-11, worker-owned throwaway JIT registrations
158/159, both deleted): a second `POST {server_url_v2}/session` while a session
exists returns **409 with an empty body** (`content-length: 0`). After the
holder session was deleted, `POST /session` kept returning 409 for ~181 s, then
succeeded. That lingering-session window is exactly what toolu's `run` loop hits
when it retries the *same* JIT config after a transient listener error.

## Non-Goals

1. No re-mint on conflict — the issue requires bounded retry then a distinct exit.
2. No reclassification of other session-create statuses (401/403/404/5xx stay `Protocol`).
3. No change to the existing `--once` generic-failure exit (main maps it to 2).
4. No launchd behaviour change: launchd `KeepAlive` cannot condition restart on an exit status; documented as a platform limit.
5. GHES V1 session path is not separately exercised; the same `create_session` transport serves both, but no GHES endpoint is available (unverified, not claimed).

## Architecture

1. **Classify at the transport.** `wire::net::create_session` returns
   `RunnerError::SessionConflict` for HTTP 409 (body never logged/echoed). All
   other non-2xx keep `Protocol`.
2. **Bounded retry in the listener.** New `listener::session_conflict` module:
   `SessionConflictPolicy { interval, max_attempts }` (default 30 s × 8 = upstream's
   4 min budget) and `retry_session_conflict(op, cancel, policy)` that re-runs
   `op` (authenticate + create_session, so a token can never age out across the
   window) only while it returns `SessionConflict`, sleeping `interval`
   cancel-aware. Returns `Ok(Some(v))` on success, `Ok(None)` when cancel fires
   during a sleep, the last `SessionConflict` once `max_attempts` attempts all
   conflicted, and any other error unchanged (its own classification).
   `GitHubListener::run` returns `Ok(None)` for the cancelled case — the same
   result as cancellation while long-polling with no job (exit 0 everywhere).
3. **Distinct exit everywhere.** `shared::SESSION_CONFLICT_EXIT_CODE = 5`
   (upstream parity). `listener::loop_decision::next_action` maps
   `Err(SessionConflict)` → `Exit(5)` under `--once` and in the default loop
   (after cancel/pending-remove precedence), so the outer loop can never turn
   the bounded policy into generic backoff. `run_cmd` carries code 5 to `main`
   through a typed `CodedExit` error that `main` downcasts; `boot`'s
   `exit_code_for` maps it to 5.
4. **Supervisor.** `config::service_unit::systemd_unit` adds
   `RestartPreventExitStatus=5` so systemd does not restart on a conflict
   (upstream's "stop the service"). launchd is unchanged and documented.
5. **Timing knob for tests/operators.** `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS`
   (read only in `toolu-runner/src/config.rs`) overrides the interval; attempts
   stay 8. Lets the real binary prove exits in seconds. Invalid values are
   diagnosed and ignored (default used).

Decisions (Jev-checked, 0.95–0.99): systemd-prevent + document launchd;
cancel → `Ok(None)`; upstream 30 s / 4 min timing.

## Interfaces / Schema

```rust
// shared/src/error.rs
#[error("another runner session is active for this registration: {0}")]
SessionConflict(String),            // new RunnerError variant
pub const SESSION_CONFLICT_EXIT_CODE: i32 = 5;

// listener/src/session_conflict.rs
pub struct SessionConflictPolicy { pub interval: Duration, pub max_attempts: u32 }
impl Default for SessionConflictPolicy  // 30 s, 8
pub(crate) async fn retry_session_conflict<T, F, Fut>(op: F, cancel: &CancellationToken,
  policy: SessionConflictPolicy) -> Result<Option<T>, RunnerError>;

// listener/src/handler.rs
impl GitHubListener { pub fn with_session_conflict_policy(self, p: SessionConflictPolicy) -> Self }

// toolu-runner/src/config.rs
pub(crate) fn session_conflict_interval_ms() -> Option<String>; // TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS
```

Systemd unit gains the line `RestartPreventExitStatus=5` after `RestartSec=5`.
Captured fixture `crates/wire/tests/fixtures/session_conflict_409.json`:
`{status, headers, body}` from the live capture (request id/date kept; they are
not secrets).

## Failure modes and edge cases

| Input | Observable behaviour |
| --- | --- |
| 409 then 200 within 8 attempts | WARN per conflict (`attempt n/8`, no token/body); session created; job polling continues. |
| 409 on all 8 attempts | ERROR "session conflict persisted …"; `run` (loop and `--once`) and `boot` exit **5**; systemd does not restart. |
| 401/403/404/5xx during or before retry | Not retried by this policy; returned with existing classification (`Protocol`/`Auth`/`Network` from auth). |
| SIGINT/SIGTERM during the retry sleep | Wakes immediately; `Ok(None)`; `run`/`boot` exit 0. |
| Cancellation already set when lifecycle starts | First attempt still runs (existing behaviour); a conflict then returns `Ok(None)` without sleeping. |
| Cancel during an in-flight auth/session HTTP request | Not interrupted (existing pre-poll behaviour; bounded by the 60 s client timeout); observed at the next sleep. |
| 409 then 401/403/500 inside the window | Retry stops; that error is returned with its own classification. |
| `.pending_remove` present when conflict ends | Existing precedence: exit 0. |
| Knob unset/empty/non-numeric/0 | Default 30 s used (0 and garbage diagnosed via WARN). |
| launchd service | Relaunched by `KeepAlive`; each relaunch is one fresh bounded window (documented). |

## Acceptance criteria

- **AC-1:** Given the captured live 409 (empty body), `wire::net::create_session` returns `RunnerError::SessionConflict` whose Display contains "another runner session is active for this registration"; 401, 403 and 500 responses still return `RunnerError::Protocol`.
- **AC-2:** Given a broker that returns the captured 409 once and then a valid session, `GitHubListener::run` creates the session after one retry and proceeds (91-S1).
- **AC-3:** Given a persistent captured 409, the listener makes exactly `max_attempts` session attempts and returns `SessionConflict`; given the 409 followed by a 500 (and by a 401), it stops retrying and returns `Protocol` (91-S2).
- **AC-4:** Given a persistent 409 and cancellation during the retry sleep, the listener returns `Ok(None)` promptly (well under one interval) (91-S2).
- **AC-5:** `next_action(Err(SessionConflict), …)` returns `Exit(5)` with and without `--once`; cancellation still yields `Exit(0)`; other errors keep their actions (91-S3).
- **AC-6:** The real `toolu-runner` binary exits 5 on a persistent conflict for `boot`, `run --once`, and the default `run` loop (no generic backoff), with a WARN line containing `session conflict` and `attempt 1/8` and a final error naming "another runner session is active for this registration", and the access token / JIT blob absent from stderr and the diag log (91-S2/S3).
- **AC-7:** The rendered systemd unit and the legacy `scripts/toolu-runner.service` both contain `RestartPreventExitStatus=5` (91-S3 service restart).
- **AC-8:** `./tools/check.sh all` passes unmodified.
- **AC-9:** The committed fixture is the live capture verbatim (status 409, `content-length: 0`, empty body) with a `provenance` block (date, request path, throwaway registration ids, capture method); tests load it rather than constructing the response.

## Acceptance evidence

| AC | Real input | Check |
| --- | --- | --- |
| AC-1 | `crates/wire/tests/fixtures/session_conflict_409.json` (live capture) served from wiremock; 401/403/500 variants | `cargo test -p wire --test session_conflict` |
| AC-2–4 | Same fixture via wiremock broker + real RSA JIT envelope, millisecond policy | `cargo test -p listener --lib session_conflict` (sibling `src/tests/session_conflict.rs`) |
| AC-5 | Pure decision | `cargo test -p listener --test loop_decision_test` |
| AC-6 | Real binary, wiremock broker serving the capture, `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS=50` | `cargo test -p toolu-runner --test session_conflict_e2e_test` |
| AC-7 | Rendered unit + legacy unit file | `cargo test -p config --lib service_unit` (sibling test) and `cargo test -p toolu-runner --test install_service_test` |
| AC-8 | Whole workspace | `./tools/check.sh all` |
| AC-9 | Fixture file | `cargo test -p wire --test session_conflict` (`fixture_is_the_verbatim_live_capture`) |
| Live | github.com JIT registration, real broker | Recorded capture + 181 s recovery observation in `docs/test-coverage.md`; reference-runner comparison is upstream source only (unverified live). |

## Documentation impact

`AGENTS.md` (loop decision, boot exit codes, install-service unit, wire session),
`docs/architecture.md` (session-create conflict handling), `docs/container-image.md`
(boot exit-code table gains `5`), `scripts/toolu-runner.service` (legacy unit), `docs/test-coverage.md`
(new #91 section mapping ACs/scenarios and unverified lanes), CLI `--help`
Environment footer unchanged (knob is documented in architecture docs only).

## Open Questions

None blocking. GHES and an official-runner live comparison remain unverified and
are recorded as such.

## Spec review (2026-10-11)

- Documentation impact: 🟡 should-fix: container exit-code table and legacy unit missing. Fixed (added).
- Acceptance evidence: 🟡 should-fix: fixture provenance unasserted. Fixed (AC-9).
- AC-3: 🟡 should-fix: unrelated failure inside the window not covered. Fixed (409→500/401).
- AC-6: 🟡 should-fix: log assertion vague. Fixed (exact WARN/error substrings, token absence).
- Failure modes: 🔵 consider: cancellation during in-flight request. Fixed (row added).
Jev coverage check (noul 0.6, weakest = live capture) addressed by AC-9. No blockers remain.
