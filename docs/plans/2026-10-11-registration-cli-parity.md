# Registration CLI parity: `--ephemeral` and `--no-default-labels` — Plan

**Date:** 2026-10-11   **Status:** Approved   **Spec:** docs/specs/2026-10-11-registration-cli-parity-design.md   **Topic:** issue #92

## Evidence and approach

Inspected: `crates/toolu-runner/src/{cli,register_cmd,run_cmd,main,status_cmd,service_cmd}.rs`,
`crates/listener/src/{loop_decision,handler,job_lifecycle}.rs`,
`crates/config/src/config.rs`, `crates/wire/src/net/register.rs`
(`unregister_runner`), the `boot_fixtures` / `session_conflict_e2e_test` /
`remove_unregister_test` / `register_zero_arg_test` harnesses, and upstream
`cab9d1c` `CommandSettings.cs`, `Validators.cs`, `ConfigurationManager.cs`,
`Runner.cs`. Recalled: comemory `4f1b849e` (live JIT probe recipe, 409 window),
`43e16a1f` (job lease gotchas: `job.ts -- --`, push hook blocks WIP pushes).

Approach (from the approved spec): a pure `config::labels` module; an
`ephemeral` key on `RunnerRegistrationConfig`; a pure
`listener::loop_decision::next_ephemeral_action`; a bin-side `ephemeral.rs`
that reuses `remove_bearer`, `delete_registration_state`,
`UNREGISTER_TIMEOUT` and `wire::net::unregister_runner`; `run_cmd::RunLoop`
branches on `cfg.ephemeral`; `install-service` refuses; `status` prints the
mode. Size: `run_cmd.rs` (408 lines) gains only a branch; the cleanup lives in
`ephemeral.rs`.

## Workstream summary

config (labels + field) → listener decision → register CLI → ephemeral run
lifecycle → install-service/status → live lane → docs → gate.

## Steps (machine-readable)

```json
[
  {
    "id": "config_labels",
    "title": "config::labels (defaults via shared::platform, additive merge, empty-drop, case-insensitive dedupe, 256-char limit, no-default-without-labels error) + RunnerRegistrationConfig.ephemeral (#[serde(default)]) + live-probe fixture test",
    "ac_refs": ["AC-3", "AC-4"],
    "paths": [
      "crates/config/src/lib.rs",
      "crates/config/src/labels.rs",
      "crates/config/src/config.rs",
      "crates/config/tests/labels_test.rs",
      "crates/config/tests/remint_test.rs",
      "crates/config/tests/fixtures/jit_labels_probe.json"
    ],
    "input": "Live github.com generate-jitconfig probe of 2026-10-11 (runners 163/164 on Falconiere/toolu-ghrunner): requested label lists and the label sets GitHub stored, plus the 422 for an empty list; a pre-#92 config.toml text without the ephemeral key.",
    "check": "cargo test -p config --test labels_test --test remint_test"
  },
  {
    "id": "ephemeral_decision",
    "title": "Pure listener::loop_decision::next_ephemeral_action + EphemeralAction with the spec's precedence table",
    "ac_refs": ["AC-8"],
    "paths": [
      "crates/listener/src/loop_decision.rs",
      "crates/listener/tests/loop_decision_test.rs"
    ],
    "input": "Every outcome shape the listener returns: Ok(Some(Success|Failure|Cancelled)), Ok(None), Err(SessionConflict), Err(Auth), Err(Network), crossed with once / cancelled / pending_remove.",
    "check": "cargo test -p listener --test loop_decision_test"
  },
  {
    "id": "register_flags",
    "title": "register --no-default-labels / --ephemeral; --labels additive through config::labels; ephemeral persisted; struct literals updated; real-binary register tests",
    "ac_refs": ["AC-1", "AC-2", "AC-4"],
    "depends_on": ["config_labels"],
    "paths": [
      "crates/config/src/labels.rs",
      "crates/config/src/config.rs",
      "crates/toolu-runner/src/cli.rs",
      "crates/toolu-runner/src/register_cmd.rs",
      "crates/toolu-runner/tests/register_labels_test.rs",
      "crates/toolu-runner/tests/fixtures/generate_jitconfig_response.json",
      "crates/toolu-runner/tests/config_resolve_test.rs",
      "crates/toolu-runner/tests/install_service_test.rs",
      "crates/toolu-runner/tests/remove_unregister_test.rs",
      "crates/toolu-runner/tests/run_loop_warn_test.rs",
      "crates/toolu-runner/tests/session_conflict_e2e_test.rs",
      "crates/toolu-runner/tests/storage_layout_test.rs"
    ],
    "input": "The real toolu-runner binary run with --labels gpu; --no-default-labels --labels gpu,GPU,,tpu; --labels ''; --no-default-labels alone; a 257-char label; --ephemeral — against a local generate-jitconfig endpoint serving the committed real-envelope fixture, recording each request body.",
    "check": "cargo test -p toolu-runner --test register_labels_test --test config_resolve_test --test remove_unregister_test --test storage_layout_test"
  },
  {
    "id": "ephemeral_run",
    "title": "ephemeral.rs post-job unregister + local cleanup; RunLoop branch on cfg.ephemeral (no re-mint, no login warning); real-binary run lifecycle tests",
    "ac_refs": ["AC-5", "AC-6", "AC-7", "AC-12"],
    "depends_on": ["ephemeral_decision", "register_flags"],
    "paths": [
      "crates/listener/src/loop_decision.rs",
      "crates/wire/src/net/register.rs",
      "crates/toolu-runner/src/main.rs",
      "crates/toolu-runner/src/run_cmd.rs",
      "crates/toolu-runner/src/ephemeral.rs",
      "crates/toolu-runner/tests/ephemeral_run_test.rs",
      "crates/toolu-runner/tests/helpers/boot_fixtures.rs",
      "crates/toolu-runner/tests/fixtures/job_message.json",
      "crates/toolu-runner/tests/session_conflict_e2e_test.rs",
      "crates/toolu-runner/tests/run_loop_warn_test.rs"
    ],
    "input": "Real JIT envelope with a fresh 2048-bit RSA keypair; the captured job message fixtures/job_message.json cut to one real shell step (`true`, `exit 3`, `sleep 30` + SIGTERM); a persisted ephemeral registration whose runner_url and broker point at a local HTTP endpoint; DELETE answering 204 / 500 / 404 + empty runner list; OAuth token exchange answering 401; no token.",
    "check": "cargo test -p toolu-runner --test ephemeral_run_test --test session_conflict_e2e_test --test run_loop_warn_test"
  },
  {
    "id": "service_status",
    "title": "install-service refuses an ephemeral registration (default and --print; --remove allowed); status prints the mode",
    "ac_refs": ["AC-9"],
    "depends_on": ["register_flags"],
    "paths": [
      "crates/toolu-runner/src/service_cmd.rs",
      "crates/toolu-runner/src/status_cmd.rs",
      "crates/toolu-runner/tests/install_service_test.rs",
      "crates/toolu-runner/tests/config_resolve_test.rs",
      "crates/toolu-runner/tests/cli_test.rs"
    ],
    "input": "A persisted config.toml with ephemeral = true and one without the key, driven through the real binary.",
    "check": "cargo test -p toolu-runner --test install_service_test --test config_resolve_test --test cli_test"
  },
  {
    "id": "live_lane",
    "title": "Live github.com lane: real ephemeral toolu registration runs one dispatched job and the runner is gone afterwards; --no-default-labels registration shows exact labels; evidence recorded and re-checkable",
    "ac_refs": ["AC-10"],
    "depends_on": ["ephemeral_run"],
    "paths": [
      "scripts/test/ephemeral_92_live_check.sh",
      "crates/toolu-runner/tests/fixtures/ephemeral_92_live.json"
    ],
    "input": "github.com Falconiere/toolu-ghrunner: `toolu-runner register --ephemeral --labels <unique>` then `run`, with post-results-live.yml / noop-live.yml dispatched to that label; GET /actions/runners/{id} after exit.",
    "check": "bash scripts/test/ephemeral_92_live_check.sh"
  },
  {
    "id": "docs",
    "title": "README flags table + config example, --help text, AGENTS.md module/run-loop notes, docs/test-coverage.md #92 section (every AC and 92-S1..S4, platform/GHES applicability)",
    "ac_refs": ["AC-1", "AC-5", "AC-9", "AC-10"],
    "depends_on": ["service_status", "live_lane"],
    "paths": [
      "README.md",
      "AGENTS.md",
      "docs/test-coverage.md",
      "crates/toolu-runner/src/cli.rs"
    ],
    "input": "The implemented flags, exit codes, and the recorded live run ids.",
    "check": "grep -q -- '--no-default-labels' README.md && grep -q -- '--ephemeral' README.md && grep -q 'ephemeral.rs' AGENTS.md && grep -q 'config/labels.rs\\|labels.rs' AGENTS.md && grep -q '## Registration CLI parity (#92)' docs/test-coverage.md"
  },
  {
    "id": "gate",
    "title": "Full repository gate",
    "ac_refs": ["AC-11"],
    "depends_on": ["config_labels", "ephemeral_decision", "register_flags", "ephemeral_run", "service_status", "live_lane", "docs"],
    "paths": ["crates", "scripts", "docs", "README.md", "AGENTS.md", "Cargo.lock"],
    "input": "Whole workspace.",
    "check": "./tools/check.sh all"
  }
]
```

## Critical files

- Create: `crates/config/src/labels.rs`, `crates/config/tests/labels_test.rs`,
  `crates/config/tests/fixtures/jit_labels_probe.json`,
  `crates/toolu-runner/src/ephemeral.rs`,
  `crates/toolu-runner/tests/register_labels_test.rs`,
  `crates/toolu-runner/tests/ephemeral_run_test.rs`,
  `crates/toolu-runner/tests/fixtures/ephemeral_92_live.json`,
  `scripts/test/ephemeral_92_live_check.sh`.
- Modify: `crates/config/src/{lib,config}.rs`, `crates/listener/src/loop_decision.rs`,
  `crates/listener/tests/loop_decision_test.rs`,
  `crates/toolu-runner/src/{cli,register_cmd,run_cmd,main,service_cmd,status_cmd}.rs`,
  the seven test files that build `RunnerRegistrationConfig` literals,
  `README.md`, `AGENTS.md`, `docs/test-coverage.md`.

## Verification

- Red first: `register_labels_test` (additive / `--no-default-labels`) and
  `ephemeral_run_test` fail on the unchanged code (unknown flag; a re-mint or
  a second poll instead of cleanup); recorded before implementation.
- Green: every step check above, then the live lane (run ids + runner 404
  recorded in `ephemeral_92_live.json`, re-verified by the checker against
  the GitHub API), then `./tools/check.sh all`.
- Failure/boundary: the spec's failure table rows each map to a named test
  (AC-2, AC-6, AC-7, AC-12).
- Docs: README, `--help`, AGENTS.md and `docs/test-coverage.md` updated in the
  `docs` step; GHES and macOS recorded as unverified.
- Delivery: scoped commit, `plan-ledger.js run <plan> --verify` against the
  final diff, `toolu-review:review` (v2 push-review state), `verdict.js status`
  = `overall: ready`, then push, PR to `main`, `pr-babysit:babysit`.
  Preflight prerequisites: `gh api user` OK, branch
  `feat/92-registration-cli-parity-ephemeral-and` (non-default).

## Deviations

- `service_status`: the `status` mode test lives in
  `config_resolve_test.rs` (it already owns the `status` fixtures) instead of
  `cli_test.rs`; the step's paths and check now include it.
- `ephemeral_run`: a SIGTERM-stopped job is reported as `failed` (toolu's
  existing operator-shutdown result), not `canceled`; the test pins the
  observed value and #92 leaves that behavior unchanged.

## Plan review (2026-10-11)

Round 1 — Needs changes:

```
register_flags: 🟡 should-fix: check compiles and exercises crates/config/src/{labels,config}.rs but they were not in paths (stale-green risk). Added.
ephemeral_run: 🟡 should-fix: check reads crates/listener/src/loop_decision.rs and crates/wire/src/net/register.rs (unregister_runner) but they were not in paths. Added.
Verification: 🟡 should-fix: final ledger --verify, review, verdict and PR/babysit delivery prerequisites were not stated. Added.
```

Round 2 — every spec AC (1–12) maps to a step's `ac_refs`; each behavior
step names a real input, failure/boundary cases and a runnable check; order
config → listener → register → run → service/status → live → docs → gate is
acyclic; docs step has a runnable check; delivery path stated.
