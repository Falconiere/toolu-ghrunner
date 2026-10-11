# Session-create 409 conflicts — Plan

**Date:** 2026-10-11   **Status:** Approved   **Spec:** docs/toolu/specs/2026-10-11-session-create-conflict-design.md   **Topic:** Classify 409, bounded retry, exit 5

## Evidence and approach

- Live github.com capture (2026-10-11, throwaway JIT runners 158/159, deleted):
  409 with empty body; lingering 409 clears ~181 s after the holder session is
  deleted. Upstream `cab9d1c`: 30 s × 8 attempts (4 min), `ReturnCode.SessionConflict = 5`,
  service stops on 5.
- Reused paths: `wire::net::session::create_session` (classification),
  `listener::retry` pattern (`retry_transient`, cancel-aware sleep),
  `listener::loop_decision::next_action`, `toolu-runner` `config.rs` env
  accessors, `boot_cmd::exit_code_for`, `config::service_unit::systemd_unit`,
  `tests/helpers/boot_fixtures.rs` (real RSA JIT envelope + wiremock broker).
- Recalled memories: job.ts `-- --` quirk and capacity-1 lease retries
  (43e16a1f, ced17f92).

## Workstream summary

shared error/exit code → wire classification + live fixture → listener bounded
retry + handler wiring → loop decision → bin exit plumbing + knob + e2e →
service units → docs → full gate.

## Steps (machine-readable)

```json
[
  {
    "id": "S1-shared-variant",
    "title": "shared: RunnerError::SessionConflict(String) (Display names 'another runner session is active for this registration') + pub const SESSION_CONFLICT_EXIT_CODE = 5; workspace still compiles",
    "check": "cargo test -p shared && cargo check --workspace --all-targets",
    "ac_refs": ["AC-1", "AC-5"],
    "input": "upstream Constants.cs ReturnCode.SessionConflict = 5",
    "paths": ["crates/shared/src"],
    "model": "inherit"
  },
  {
    "id": "S2-wire-classify",
    "title": "wire: create_session maps HTTP 409 to SessionConflict (body never logged); committed verbatim live-capture fixture with provenance; sibling integration test (409 -> SessionConflict, 401/403/500 -> Protocol, fixture verbatim)",
    "check": "cargo test -p wire --test session_conflict",
    "ac_refs": ["AC-1", "AC-9"],
    "depends_on": ["S1-shared-variant"],
    "input": "crates/wire/tests/session_conflict_409.json (live github.com capture: status 409, content-length 0, empty body); test fixture_is_the_verbatim_live_capture asserts those fields plus the provenance block (date, request path, deleted registration ids, method); 409 responses served from the file, never constructed",
    "paths": ["crates/wire/src/net/session.rs", "crates/wire/tests/session_conflict.rs", "crates/wire/tests/session_conflict_409.json", "crates/wire/Cargo.toml"],
    "model": "inherit"
  },
  {
    "id": "S3-listener-retry",
    "title": "listener: session_conflict module (SessionConflictPolicy default 30s x 8, retry_session_conflict cancel-aware -> Ok(None) on cancel); handler retries authenticate+create_session through it, run() returns Ok(None) when cancelled mid-retry; GitHubListener::with_session_conflict_policy; sibling tests over real wire::net::create_session + wiremock serving the capture",
    "check": "cargo test -p listener --lib session_conflict",
    "ac_refs": ["AC-2", "AC-3", "AC-4"],
    "depends_on": ["S2-wire-classify"],
    "input": "crates/wire/tests/session_conflict_409.json; boundaries: 409->200 (Some, 2 requests), persistent 409 (wiremock .expect(8) exact attempt count, SessionConflict), 409->500 and 409->401 (Protocol, exactly 2 requests), cancel during a 30 s sleep (Ok(None) in < 2 s)",
    "paths": ["crates/listener/src/session_conflict.rs", "crates/listener/src/handler.rs", "crates/listener/src/lib.rs", "crates/listener/src/tests/session_conflict.rs", "crates/listener/src/tests/README.md", "crates/wire/tests/session_conflict_409.json"],
    "model": "inherit"
  },
  {
    "id": "S4-loop-decision",
    "title": "listener loop_decision: Err(SessionConflict) -> Exit(SESSION_CONFLICT_EXIT_CODE) under --once and in the default loop (after cancel / pending_remove precedence); table tests",
    "check": "cargo test -p listener --test loop_decision_test",
    "ac_refs": ["AC-5"],
    "depends_on": ["S1-shared-variant"],
    "input": "real RunnerError::SessionConflict outcome rows alongside existing Done/Net/Proto/Auth rows",
    "paths": ["crates/listener/src/loop_decision.rs", "crates/listener/tests/loop_decision_test.rs"],
    "model": "inherit"
  },
  {
    "id": "S5-bin-exit",
    "title": "toolu-runner: TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS accessor in config.rs -> policy for run and boot; run_cmd finish returns a typed CodedExit(5) that main downcasts; boot exit_code_for maps SessionConflict -> 5; real-binary e2e (boot recover->0, boot persistent->5, boot SIGTERM mid-retry->0 fast, run --once ->5, default run loop ->5, WARN/error text, token/JIT blob absent from stderr and diag log)",
    "check": "cargo test -p toolu-runner --test session_conflict_e2e_test",
    "ac_refs": ["AC-6"],
    "depends_on": ["S3-listener-retry", "S4-loop-decision"],
    "input": "live-capture fixture served by wiremock + real RSA JIT envelope (boot_fixtures) + persisted registration config pointing at it; assertions: exit status code exactly 5 (boot, run --once, default run), session POST count exactly 8 for each persistent case and no generate-jitconfig re-mint (proves no generic backoff/re-mint), stderr contains `session conflict` + `attempt 1/8` + `another runner session is active for this registration`, the fake access token and the JIT blob absent from stderr and _diag log; recover case exits 0 after a completed job; SIGTERM mid-retry (default 30 s interval) exits 0 in < 5 s",
    "paths": ["crates/toolu-runner/src", "crates/config/src", "crates/toolu-runner/tests/session_conflict_e2e_test.rs", "crates/toolu-runner/tests/helpers/boot_fixtures.rs", "crates/wire/tests/session_conflict_409.json", "crates/listener/src", "crates/wire/src"],
    "model": "inherit"
  },
  {
    "id": "S6-service-units",
    "title": "systemd_unit renders RestartPreventExitStatus=5 (from shared constant); the five systemd_*.service golden fixtures and legacy scripts/toolu-runner.service gain the same line; service_unit_test asserts the golden renders and the legacy file; install_service_test still passes",
    "check": "cargo test -p config --test service_unit_test && cargo test -p toolu-runner --test install_service_test",
    "ac_refs": ["AC-7"],
    "depends_on": ["S1-shared-variant"],
    "input": "rendered unit for a real ServiceSpec; scripts/toolu-runner.service file contents",
    "paths": ["crates/config/src/service_unit.rs", "crates/config/tests/service_unit_test.rs", "crates/config/tests/fixtures/service", "scripts/toolu-runner.service", "crates/toolu-runner/tests/install_service_test.rs", "crates/toolu-runner/src/service_cmd.rs"],
    "model": "inherit"
  },
  {
    "id": "S7-docs",
    "title": "Docs: AGENTS.md (wire session, loop_decision, boot exit codes, install-service), docs/architecture.md (conflict handling + knob), docs/container-image.md (exit 5), docs/test-coverage.md (#91 section with scenario map, live capture, unverified lanes)",
    "check": "grep -q 'SessionConflict' AGENTS.md && grep -q 'RestartPreventExitStatus=5' AGENTS.md && grep -q '| `5` |' docs/container-image.md && grep -q 'TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS' docs/architecture.md && grep -q '## Session-create conflicts (#91)' docs/test-coverage.md && grep -q '91-S3' docs/test-coverage.md",
    "ac_refs": ["AC-6", "AC-7", "AC-9"],
    "depends_on": ["S5-bin-exit", "S6-service-units"],
    "paths": ["AGENTS.md", "docs/architecture.md", "docs/container-image.md", "docs/test-coverage.md"],
    "model": "inherit"
  },
  {
    "id": "S8-gate",
    "title": "Full repository gate, unmodified",
    "check": "./tools/check.sh all",
    "ac_refs": ["AC-8"],
    "depends_on": ["S7-docs"],
    "paths": ["crates", "scripts", "AGENTS.md", "docs", "Cargo.toml", "Cargo.lock", "tools"],
    "model": "inherit"
  }
]
```

## Critical files

- `crates/shared/src/error.rs`
- `crates/wire/src/net/session.rs`, `crates/wire/Cargo.toml`, `crates/wire/tests/session_conflict.rs`, `crates/wire/tests/session_conflict_409.json`
- `crates/listener/src/session_conflict.rs`, `handler.rs`, `lib.rs`, `loop_decision.rs`, `src/tests/session_conflict.rs`, `src/tests/README.md`, `tests/loop_decision_test.rs`
- `crates/toolu-runner/src/{config.rs,run_cmd.rs,boot_cmd.rs,main.rs}`, `tests/session_conflict_e2e_test.rs`, `tests/install_service_test.rs`
- `crates/config/src/service_unit.rs`, `crates/config/tests/service_unit_test.rs`, `crates/config/tests/fixtures/service/systemd_*.service`, `scripts/toolu-runner.service`
- `AGENTS.md`, `docs/architecture.md`, `docs/container-image.md`, `docs/test-coverage.md`

## Verification

Real inputs: the live 409 capture (verbatim), real RSA JIT envelopes, the
production transport/listener/binary. Boundaries: recover-within-window,
persistent, unrelated 401/500 inside the window, SIGTERM mid-sleep, `--once`,
default loop, pending-remove precedence (table test). Docs synced in S7;
`./tools/check.sh all` in S8. GHES and an official-runner live comparison remain
recorded as unverified.

## Plan review (2026-10-11)

- S6: 🔴 blocker: check targeted install_service_test while unit text is pinned by config's service_unit_test golden fixtures. Fixed (check + paths + fixtures).
- S5: 🟡 should-fix: "no generic backoff" for the default loop was not observable. Fixed (exact session POST count, no re-mint request).
- S2: 🟡 should-fix: provenance assertion unnamed. Fixed (named test + fields).
- S3: 🔵 consider: "promptly" unquantified. Fixed (< 2 s against a 30 s interval).
`pl_check_ac_refs`: rc=0, every AC-1..AC-9 mapped. Jev alignment 0.58 (weakest AC-6/AC-9) before fixes; both strengthened. Delivery: scoped commits, ledger `--verify`, toolu-review, verdict, push, PR, babysit per execution reference.
