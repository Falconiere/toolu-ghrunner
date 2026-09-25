# Conditional cleanup — Plan

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/specs/2026-09-25-conditional-cleanup-design.md   **Topic:** #100

## Evidence and approach

steps_runner.rs returns hard errors before result adjustment and exits on cancel.
Pinned StepsRunner.cs separates shutdown and graceful cancellation. Existing
step_timeout, post_drain, captured #68 messages, and process-group composite code
provide reusable paths. Keep setup failures fatal and use one shared cleanup bound.

## Workstream summary

Captured replay regressions and result policy → cancellation control and process
ownership → docs, full gate, platform/reference verification and authorized PR.

## Steps (machine-readable)

```json
[
  {
    "id": "gate",
    "title": "Verify error policy, cancellation, posts, listener integration and all workspace gates",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "paths": [
      "."
    ],
    "input": "Captured acquired jobs, real shell/Node/process-family probes, unchanged complete workspace regressions; includes conditional_cleanup_test, post_results_test, step_timeout and listener tests",
    "check": "git diff --check && ./tools/check.sh all"
  },
  {
    "id": "live",
    "title": "Validate evidence inventory with concrete unavailable-lane reasons",
    "depends_on": [
      "gate"
    ],
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "paths": [
      "."
    ],
    "input": "Recorded real Docker lifecycle passes; GitHub runner inventory; orchestrator-confirmed absent GHES; all remaining live comparisons explicitly unverified",
    "check": "python3 scripts/test/conditional_cleanup_evidence_check.py --local"
  }
]
```

## Critical files

Modify crates/execution/src/execution/steps_runner.rs, step_errors.rs,
step_timeout.rs, cgroup_join.rs, post_drain.rs, node_stage.rs, context.rs, job_runner.rs,
job_runner/prepared.rs, job_runner/entry.rs, lib.rs and crates/listener/src/execution_loop.rs.
Add execution/job_cancellation.rs, conditional_cleanup_test.rs and evidence
manifest/checker. Update touched module READMEs, README.md, docs/architecture.md
and docs/test-coverage.md. Reuse the captured incoming_contexts_matrix_0.json and
post_results action fixtures. Split control helpers under existing steps_runner/
if necessary to keep production files within 500 code lines.

## Verification

The ledger requires real tools and exact markers, outcome/conclusion, report UUIDs,
completion cardinality and unrelated-process survival. Preserve action download
cancellation and setup teardown behavior. Full gate cannot be disabled. Linux,
macOS, reference, GitHub.com/GHES and actual CLI SIGTERM are separately recorded;
missing lanes remain unverified with concrete reasons. The default evidence checker
continues to reject incomplete parity certification; delivery checks use --local. Before authorized
commit/push, verify ledger against final diff, perform toolu-review, fetch/rebase
if main moved, rerun gate, and require green verdict. PR body starts with Closes
Falconiere/toolu-ghrunner#100 and Part of Falconiere/toolu-ghrunner#67. Babysit to
zero findings and green CI; report ready without merging.

## Plan review

Approved: every AC maps to runnable real-input checks, paths cover dependencies,
and the evidence inventory records missing required lanes without certifying them. New file paths above
are planned creations. Execution stays inline; no delegation required.

## Deviations

Local review found Node runtime resolution was unbounded after removing the
outer post-stage timeout. Apply existing StepBounds to pre-process runtime/env
preparation, while still awaiting subprocess reaping. This is required by AC-4.

The workspace gate exposed a legacy `gh_compat_prepost` expectation that local
action resolution escapes as Err. Update that assertion to exact Ok(Failure),
preserving the real Node post-state assertion, under the docs_gate regression
scope. Its six tests and the unchanged full Linux gate passed. Live acceptance remains incomplete where environments are unavailable.

## Orchestrator delivery decision — 2026-09-25

The orchestrator explicitly directed delivery with unavailable lanes marked
unverified. There is no GHES endpoint or credential for this epic run; GHES
cannot be exercised. Linux full gate plus GitHub ci/ci-macos are authoritative
when macOS process startup stalls in _dyld_start. This supersedes the original
live-evidence delivery prerequisite, without treating unverified lanes as passed.

Available live Docker probe: all four existing job_container_failures tests passed
against real Colima, including cancellation/timeout post execution and removal.
Full mixed-service/sentinel comparison is still unverified. No online paired
issue-100 runner exists for GitHub.com/reference or acquired-job SIGTERM; the API
lists only offline toolu-70-final. GHES has no endpoint/credential by explicit
orchestrator confirmation. These are recorded gaps, not passing lanes.

Final plan audit: use PUSH_REVIEW_BASE=origin/main because local main is stale.
Consolidated overlapping policy/cancel/docs runners into the unchanged full
workspace gate, which contains every named regression and all original gate
layers. No test or gate is omitted; explicit live Docker results stay recorded
separately. Re-reviewed: AC-1 through AC-5 map to the complete gate and evidence
inventory. This avoids stamping already-merged sibling changes as this PR.
