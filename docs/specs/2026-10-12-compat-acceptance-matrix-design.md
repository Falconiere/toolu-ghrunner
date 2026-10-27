# Compatibility acceptance matrix and reference-runner lane — Design

> Historical implementation record. Current behavior is documented in
> [architecture](../architecture.md); acceptance status and remaining gaps are
> tracked in [test coverage](../test-coverage.md) and the
> [compatibility matrix](../compatibility-matrix.md).

**Date:** 2026-10-12   **Status:** Approved   **Author:** epic worker (#103)   **Topic:** epic #67 evidence matrix, pinned official-runner comparison, capture/replay, honest live lanes

## Problem

Epic #67's 35 other children each carry acceptance criteria (ACs) and scenario
IDs, and each recorded evidence in its own way: 11 per-issue `*_evidence.json`
files (only #88's checker runs in CI), prose sections in
`docs/test-coverage.md`, and run links scattered across PR bodies. Nothing maps
every criterion to its real input, expected observable, production path,
test/command, lane applicability and evidence, so a reader cannot tell
verified behaviour from planned or skipped behaviour. Several existing
surfaces are actively misleading:

- `crates/toolu-runner/tests/live_e2e.rs::multi_step_job` claims Docker
  coverage, but its "docker" step only `echo`es a `docker://` string. The inline
  expression workflow prints `inputs.who` and never asserts it.
  `docs/test-coverage.md` row 3 repeats the Docker claim.
- `require_live_env!` returns early, so a live test without credentials
  **passes**.
- `.github/workflows/live.yml` gates on `env.TOOLU_RUNNER_LIVE_TOKEN`, which is
  only set at step level. The condition is therefore always false. The repo has
  no such secret, and the workflow has still concluded `success` every day
  (e.g. run 38258082509).
- Captures were made with throwaway instrumentation ("The delivered runner has
  no raw-capture option", `job_container_message.md`), so a new capture cannot
  be reproduced.
- Nothing shows that the production replays (#68–#71) would go red if message
  setup dropped inputs/matrix/env/defaults/outputs (scenario S1).

## Non-Goals

1. No new runtime behaviour in release builds. The only production-code
   addition is a non-default `capture` cargo feature.
2. No macOS or GHES live execution. No host exists here, so those lanes stay
   `unverified` with reasons. macOS **hermetic** evidence is the existing
   `ci-macos` CI job.
3. No implementation of #94 (proxy/CA) or #97 (dead code/doc drift). Their rows
   are `pending` with the issue link unless the issue merges before this PR is
   finalized.
4. No fixes for semantic gaps found by the matrix inside this PR. A failing
   comparison is recorded as `unverified` with the observed difference and
   reported (Open Questions).
5. Windows, container hooks, OCI actions, `background:` steps, DAP,
   `snapshot:`, attestations, telemetry and AuthMigration stay out of scope
   (the epic's exclusions). The matrix records them as approved exclusions.

## Architecture

1. **Matrix as data.** `docs/compatibility-matrix.json` is the single source
   of truth: per-issue metadata plus one row per child AC/scenario.
   `scripts/test/compat_matrix_check.py` validates it deterministically. It
   renders `docs/compatibility-matrix.md` (`--write`), and `--check` fails when
   the committed Markdown is stale. CI runs `--check` beside the existing #88
   evidence step. Reused: the `is_test_fn` / sanitization idioms from
   `scripts/test/step_attrs_evidence_check.py`. Expected IDs per issue
   are extracted **deterministically** from the issue bodies
   (`--extract-ids`). `AC-<n>` is the n-th checkbox under
   `## Acceptance criteria`. Scenario IDs are the first column of each
   `| ID |` scenario table, verbatim (`75-S1`, `S1`). The ID lists are stored
   in `issues[n].ids`, so `--check` verifies row coverage offline and
   `--remote` re-extracts them to detect issue edits. The shared closure
   checklist is satisfied by the matrix itself and gets no per-issue rows.
   Extra row groups: `67/X-1..6` (the epic's "Required cross-feature
   workflows") and `103/*` (this issue's ACs and S1–S7, including the
   regression surfaces cache/artifacts/OIDC, command files/masking, listener
   acquire/renew/complete and cancellation). Rows are populated from issue
   bodies, `docs/test-coverage.md` sections and the per-issue evidence JSONs,
   and each one is verified against the code.
2. **Evidence kinds.** `gate` means the row's tests run in the default
   `cargo test --workspace` lane. It is backed by the recorded CI run of the
   base commit (ubuntu and macos-14 legs). `ci-docker` means the explicit Docker
   step in `ci.yml`. `run:<url>` is a GitHub Actions run. `pr:<url>` is a merged
   PR whose body records explicit-lane output. `doc:<path>#<anchor>` is a
   committed observation record. A skipped or ignored test is never `gate`.
3. **Capture infrastructure.** Add a `capture` feature to `listener`,
   forwarded by `toolu-runner/capture`. With it compiled in,
   `job_lifecycle::poll_and_execute` writes the decoded acquire body as
   `<data_dir>/_diag/captures/<UTC>-<plan_id>.json` (0600, directory 0700)
   right after `acquire_job`. A write failure WARNs and never affects the job.
   The feature is off by default and absent from release builds.
   `scripts/test/sanitize_capture.py` turns a raw body into a fixture. It
   replaces secret variable values, mask values and JWT-shaped strings with
   deterministic UUIDv5 placeholders, endpoint authorization parameters with
   `redacted`, and service URL hosts with `actions.example.invalid`. Keys,
   order, token types, UUIDs and `contextName` are preserved. It refuses
   output that still contains credential-shaped strings.
4. **Reference lane (S2/S3/S4).** Two committed workflows:
   `compat-reference-103.yml` (caller) and `compat-reference-103-probe.yml`
   (reusable, typed `workflow_call` inputs). The caller runs a
   `lane ∈ {toolu, official}` matrix on labels `toolu-103-<lane>`. The probe
   covers typed inputs with exact `toJSON(inputs)` assertions, a pinned
   `docker://alpine@sha256` step that writes a marker plus a step output
   derived from `/etc/alpine-release` and the input, an exact verify step,
   cache save then restore in a dependent job (`fail-on-cache-miss`), artifact
   upload then download with exact bytes, OIDC allowed (claims asserted,
   token never printed), and OIDC denied (no request URL without
   `id-token: write`). A negative caller job passes `who: ''` and must fail
   at the input assertion on both lanes. Both runners run on this Linux x86_64
   host against one Docker daemon. The official one is unmodified
   actions/runner **v2.337.0** linux-x64 (commit `cab9d1c`), registered via
   JIT. The toolu one is this branch's binary built with `capture`, forwarder
   mode, also registered via JIT. A temporary `push` trigger on this branch
   records the run and is removed before merge (the #95/#96 precedent), since
   `workflow_dispatch` needs the file on `main`.
   `scripts/test/reference_compare.py <run_id>` fetches jobs, steps and
   artifacts through `gh`. It normalizes only run/job IDs, timestamps, runner
   names and host paths. It compares per-lane job/step conclusions, the
   results JSON and the expected-failure step, and verifies that no
   probe-image container remains on the daemon. It writes
   `docs/compat-reference-103.json` (hash-pinned in the matrix).
5. **Production replay mutations (S1/S2).** A new
   `crates/execution/tests/replay_mutation_test.rs` reuses the committed
   captured messages and the real `Runner::execute_job`:
   `incoming_contexts_matrix_0.json` (inputs, matrix),
   `job_environment_layers.json` (env), `defaults_run_job.json` (defaults) and
   `job_outputs_message.json` (outputs). Each runs unmodified, where it must
   succeed with its existing observable. It then runs with exactly one
   message section dropped, where it must fail or change that observable.
   `compat_103_probe_message.json` (the new capture) gets a
   Docker-gated replay in the same CI Docker step: the unmodified run
   succeeds, the docker step swapped for an equivalent `echo` script fails
   verification, and `inputs.who` set to `""` fails the input assertion.
6. **Honest live lanes (S7).** `require_live_env!` panics with a named
   UNVERIFIED message. `live_e2e.rs` stops inlining workflows and
   `include_str!`s committed fixtures. `multi_step_job` runs a real
   `docker://` action whose output is asserted. `expression_evaluation`
   asserts the exact input and adds a dispatch with an empty input that must
   conclude `failure`. `live.yml` maps the secret at job level and fails with
   an explicit UNVERIFIED error when it is absent.

## Interfaces / Schema

`docs/compatibility-matrix.json` (schema 1):

```json
{
  "schema": 1,
  "epic": 67,
  "official_runner": {"version": "2.337.0", "commit": "cab9d1c3901e45c7705889c4f88284fdd93f4ae5"},
  "baseline": {"commit": "<main sha>", "gate": "<ci run url>", "ci_docker": "<same run url>"},
  "exclusions": ["windows", "container hooks (k8s)", "..."],
  "issues": {"68": {"title": "...", "state": "closed|open", "pr": "<url>|null", "coverage": "docs/test-coverage.md#<anchor>|null"}},
  "rows": [{
    "id": "68/AC-1", "issue": 68, "criterion": "...",
    "input": ["crates/execution/tests/incoming_contexts_matrix_0.json"],
    "expected": "...", "production_path": "execution::execution::job_runner::build_context",
    "tests": [["crates/execution/tests/incoming_contexts_test.rs", "acquired_contexts_retain_values_and_types"]],
    "command": "cargo test -p execution --test incoming_contexts_test",
    "lanes": {
      "hermetic": {"status": "passed", "evidence": "gate"},
      "github-com": {"status": "passed", "evidence": "run:https://github.com/Falconiere/toolu-ghrunner/actions/runs/123"},
      "reference": {"status": "unverified", "reason": "..."},
      "macos": {"status": "not-applicable", "reason": "..."},
      "ghes": {"status": "unverified", "reason": "no GHES instance"}
    }
  }]
}
```

Lane keys: `hermetic`, `docker`, `macos`, `github-com`, `reference`, `ghes`,
`ui-service`. Statuses: `passed` (needs `evidence`), and `unverified`,
`not-applicable` and `pending` (each needs `reason`; `pending` must link its
open issue).

`scripts/test/compat_matrix_check.py [--check | --write | --remote]`:

- `--check` (default, CI) validates the schema, issue coverage and lanes, and
  verifies the Markdown view is in sync. It exits non-zero with every
  violation listed.
- `--write` regenerates `docs/compatibility-matrix.md`.
- `--extract-ids ISSUE…` (network)
  prints the deterministic ID list for `issues[n].ids`. `--remote` (network,
  optional) checks that `issues` equals `gh api
  …/issues/67/sub_issues`, that each `run:` URL concluded as recorded, and that
  open issues are still open.

The `capture` feature has one fn:
`listener::capture::write_acquired(data_dir: &Path, plan_id: &str, body: &serde_json::Value)`.
The fn is always compiled, so its unit test runs
in the default gate. Only the **call site** in `poll_and_execute` sits behind
`#[cfg(feature = "capture")]`. The live reference run proves that call-site
wiring by producing the capture file. The fn is `pub(crate)`.

`scripts/test/sanitize_capture.py RAW OUT [--namespace NAME]` and
`scripts/test/reference_compare.py RUN_ID [--out PATH]`.

## Failure modes and edge cases

- **Missing test fn, renamed file, `#[ignore]` test claimed as `gate`, or
  duplicate row id:** the checker fails and names the row.
- **`passed` without evidence, or non-`passed` without reason:** the checker
  fails.
- **An issue with zero rows, or a child missing from `issues`:** the checker
  fails.
- **Open issue (#94/#97):** every row must be `pending` with the issue URL. If
  the issue has since closed, `--remote` fails, which prompts an update.
- **Stale Markdown:** `--check` fails with the `--write` hint.
- **Capture dir unwritable or disk full:** WARN once, the job proceeds, no
  capture file. The feature is off by default, so production is unaffected.
- **Sanitizer:** a raw body with an unexpected secret location still has the
  generic credential scan applied, and sanitization refuses (non-zero) if a
  credential-shaped string survives.
- **Reference run partly unavailable (runner offline, cache/OIDC service
  error):** `reference_compare.py` reports each mismatch. Affected rows stay
  `unverified` with the run link and the observed difference. They are never
  marked passed.
- **Lanes disagree:** recorded as `unverified` plus an Open Question/follow-up.
  This PR does not fix it (Non-Goal 4).
- **Live tests without credentials:** they panic with `UNVERIFIED: …`
  (fail). `live.yml` without the secret fails with the same wording.
- **Docker absent for the replay test:** it is ignored in the default gate and
  run explicitly in the CI Docker step. A missing daemon in the explicit lane
  fails it.

## Acceptance criteria

- **AC-1:** Given the committed `docs/compatibility-matrix.json`,
  `python3 scripts/test/compat_matrix_check.py --check` exits 0. Every
  epic-67 child has exactly one row per ID in `issues[n].ids`, which
  `--remote` confirms equals the live issue-body extraction. #94/#97 rows are
  `pending` with the issue URL. The `67/X-1..6` and `103/*` rows exist. Every
  row names input, expected, production path, tests/command and per-lane
  status.
- **AC-2:** The checker rejects (non-zero, row named) each of: a missing test
  fn, a non-test fn, an `#[ignore]` test with `gate` evidence, `passed`
  without evidence, non-passed without reason, a missing child issue, a
  duplicate id, and a stale Markdown view. This is proven by
  `scripts/test/test_compat_matrix_check.py` mutating the real matrix. CI runs
  both scripts.
- **AC-3:** The paired reference run (toolu vs. unmodified official v2.337.0,
  same workflow SHA and host) produces a `reference_compare.py` report with
  identical normalized job/step conclusions and results JSON for the
  positive probe, cache restore, artifact download and OIDC allowed/denied.
  The negative input job must fail at the same step on both lanes. Any
  difference is recorded as `unverified`.
- **AC-4:** `replay_mutation_test.rs` drops inputs, matrix, env, defaults and
  outputs, one at a time, from the committed captured messages. Each mutation
  makes the production replay fail or change the named observable, while the
  unmutated replay passes in the same test.
- **AC-5:** The captured toolu-lane probe message replays through
  `Runner::execute_job` with real Docker and succeeds. Swapping the
  `docker://` step for an `echo` script, or setting `inputs.who` to `""`,
  makes the replay fail at the verify/input step.
- **AC-6:** `write_acquired` creates exactly one 0600 file (directory 0700)
  whose JSON equals a real captured acquire body. It WARNs and returns on an
  unwritable directory. That is a default-gate unit test. The toolu reference
  lane, built `--features capture`, produces a capture of its acquired job.
  A default build has no call site, which the guardrail/grep test in AC-7's
  script also asserts. `sanitize_capture.py` output from a
  real capture passes the credential scan and preserves structure (keys, token
  types, `contextName`, UUIDs).
- **AC-7:** Without `TOOLU_RUNNER_LIVE_TOKEN`/`TOOLU_RUNNER_LIVE_REPO`, every
  `live_e2e.rs` test fails with an `UNVERIFIED` panic instead of passing.
  `live.yml` reads the secret at job level and fails with `UNVERIFIED` when
  it is absent. `live_e2e.rs` contains no inline workflow YAML. The multistep
  fixture contains a `docker://` step whose output a later step asserts, and
  the expression fixture asserts `inputs.who` exactly.
- **AC-8:** `docs/test-coverage.md` no longer claims Docker coverage for the
  echo step. It links the matrix and documents capture/sanitization,
  reference provenance (archive SHA-256, image digests, action SHAs),
  lanes/status semantics and approved exclusions.

- **AC-9:** The matrix has one `103/R-<surface>` regression row for each of
  cache, artifacts, OIDC, command files, masking, acquire, renew, complete and
  cancellation. Each row maps at least one existing default-gate test fn that
  exercises the production path, and the checker requires all nine surfaces.
  Cache, artifacts and OIDC carry the AC-3 reference run as their
  `reference` lane evidence. Acquire/renew/complete/cancellation carry the
  reference run's real acquisition, renewal, completion and the existing
  #77/#100 cancellation evidence. Accelerated-mode and offline differences
  are recorded as `unverified` or `not-applicable` with reasons.

## Acceptance evidence

| AC | Real input | Expected observable | Boundary/failure | Check |
| --- | --- | --- | --- | --- |
| AC-1 | Committed matrix built from live issue bodies and existing tests | exit 0; per-issue AC/S id coverage | open issues pending | `python3 scripts/test/compat_matrix_check.py --check`; `--remote` once |
| AC-2 | Real matrix, mutated copies in a temp dir | each mutation → non-zero naming the row | each listed defect | `python3 -m unittest scripts/test/test_compat_matrix_check.py` |
| AC-3 | Real GitHub run of `compat-reference-103.yml` on both runners | report `equal: true` per job; negative job failed at the input step on both | runner offline → unverified | `python3 scripts/test/reference_compare.py <run>` → `docs/compat-reference-103.json` |
| AC-4 | Captured messages for #68/#69/#70/#71 | unmutated success; mutated failure/changed observable | each section dropped alone | `cargo test -p execution --test replay_mutation_test` |
| AC-5 | Sanitized toolu-lane capture | success; swapped docker / empty input → failure at named step | Docker absent → explicit lane fails | `cargo test -p execution --test compat_probe_replay_test -- --ignored` (CI Docker step) |
| AC-6 | Committed real acquire body (`job_outputs_message.json` envelope) and the reference-run capture | file 0600 + JSON equal; sanitized output clean and structure-equal | unwritable dir → WARN, no panic | `cargo test -p listener capture`; `python3 -m unittest scripts/test/test_sanitize_capture.py` |
| AC-7 | Live test binary without env; `live.yml` and `live_e2e.rs` text | panic containing `UNVERIFIED`; job-level secret + failing branch; no inline YAML; docker fixture asserts output | env present but empty → still UNVERIFIED | `env -u TOOLU_RUNNER_LIVE_TOKEN cargo test -p toolu-runner --features live --test live_e2e -- --ignored noop_job_completes` exits non-zero with UNVERIFIED; new `scripts/test/live_lane_test.sh` (added to the ci.yml script list) asserts the workflow/fixture text |
| AC-9 | Matrix rows + existing tests (`cache_twirp_test.rs`, `oidc_token_test.rs`, `secret_sink_coverage_test.rs`, `watchdog_retry.rs`, `gh_compat_cancel.rs`, …) and the AC-3 run | nine surfaces present; tests are real test fns; reference lane linked | surface missing → checker fails | `compat_matrix_check.py --check` (REQUIRED_REGRESSIONS) + unit-test mutation |
| AC-8 | docs | stale claim gone; sections present | — | `compat_matrix_check.py --check` verifies doc anchors referenced by rows |

## Documentation impact

`docs/test-coverage.md` (corrected AC-3/AC-5 rows plus a #103 section),
new `docs/compatibility-matrix.{json,md}`, new `docs/compatibility-evidence.md`
(capture/sanitization procedure, reference provenance, lanes and status
semantics, exclusions), `docs/compat-reference-103.json`, and `AGENTS.md`
(a `capture` feature note in the listener section plus the matrix pointer).

## Open Questions

- **Semantic differences found by the reference run.** Owner: this worker.
  Non-blocking. They are recorded as `unverified` with evidence and listed in
  the PR body for the orchestrator to triage into follow-up issues (Non-Goal 4).
- **#94 merge timing.** Owner: orchestrator. Non-blocking. Rows stay `pending`
  unless #94 merges before finalization, in which case its PR evidence is
  mapped after a rebase.

## Spec review (2026-10-12)

- Architecture: 🔴 blocker (fixed): AC-1 could not verify per-issue ID coverage offline. IDs are now extracted deterministically and stored in `issues[n].ids`, and `--remote` re-checks them.
- Interfaces: 🔴 blocker (fixed): a feature-gated capture fn would never run in the gate. The fn is now always compiled and only the call site is gated.
- Acceptance evidence: 🟡 should-fix (fixed): the AC-7 live.yml check was vague. It is now `scripts/test/live_lane_test.sh`, wired into ci.yml.
- Acceptance criteria: 🟡 should-fix (fixed): the #103 AC-7 regression surfaces had no acceptance check (Jev r7 = 0.67). AC-9 was added.
- Jev requirement coverage (#103 ACs r1–r7): 0.95 / 0.96 / 0.95 / 0.88 / 0.82 / 0.83 / 0.67 → r7 fixed by AC-9.

**Status:** Approved

## Implementation deviations (recorded at execution)

- **Two capture fixtures.** The live run produced two captures, committed as
  `compat_103_contexts_message.json` (S1: shell-only typed inputs, matrix,
  env, defaults, outputs) and `compat_103_docker_message.json` (S2). This
  replaces the single `compat_103_probe_message.json`. No earlier capture
  carried workflow/job `env`, so the S1 mutations use the new contexts
  capture, plus the #70 capture as a second outputs witness.
- **Sanitizer.** It follows the #70/#99 convention: location-derived UUIDv5
  placeholders for every secret. Service URLs are kept, because they are not
  credentials and the replays read them.
- **Recorded differences.** The paired run found two differences, recorded
  per Non-Goal 4 (Jev record_only 0.8 / 1.0). They are listed under
  `acknowledged` in `docs/compat-reference-103.json` and keep their rows
  unverified:
  - forwarder-mode cache never enables cache service v2;
  - the official runner reports a `Pull <image>` setup step that toolu does
    not.
- **Python tests as gate evidence.** The matrix may map unittest methods
  that CI's "Compatibility evidence (#103)" step runs. They count as `gate`
  evidence on the hermetic lane only.
