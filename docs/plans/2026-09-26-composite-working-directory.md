# Composite working directory — Plan

**Date:** 2026-09-26   **Status:** Approved   **Spec:** docs/specs/2026-09-26-composite-working-directory-design.md   **Topic:** #81

## Evidence and approach

Manifest drops the field; composite_exec hardcodes workspace. Reuse #102 full
expression context/error loop and existing host/container directory dispatch.
Tests replay the captured #68 acquisition through Runner and actual Bash.

## Workstream summary

Real-shell red regression → minimal field/dispatch change → scope/failure
coverage and docs → full gate → scoped commit, committed-diff review, authorized
feature push/PR/babysit. Never merge. Fetch/rebase before code and before push.

## Steps (machine-readable)

```json
[
  {
    "id": "cwd",
    "title": "Reproduce and implement composite directory parsing and execution",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3"
    ],
    "check": "cargo test -p execution --lib composite_working_directory",
    "paths": [
      "crates",
      "Cargo.toml",
      "Cargo.lock"
    ],
    "input": "Sanitized incoming_contexts_matrix_0.json through Runner::execute_job; committed Bash actions, exact pwd/files, nested scopes and failure cleanup"
  },
  {
    "id": "docs",
    "title": "Document directory contract and measured evidence",
    "ac_refs": [
      "AC-4"
    ],
    "depends_on": [
      "cwd"
    ],
    "check": "python3 -c \"from pathlib import Path; assert 'Composite working directories (#81)' in Path('docs/test-coverage.md').read_text(); assert 'support is tracked in #81' not in Path('README.md').read_text()\"",
    "paths": [
      "README.md",
      "docs",
      "crates/execution/src/execution/tests/README.md"
    ]
  },
  {
    "id": "gate",
    "title": "Run full gate before authorized delivery",
    "ac_refs": [
      "AC-4"
    ],
    "depends_on": [
      "cwd",
      "docs"
    ],
    "check": "./tools/check.sh all",
    "paths": [
      "."
    ],
    "input": "All workspace tests, lint, format and guardrails; no suppressed failures"
  }
]
```

## Critical files

- crates/execution/src/execution/actions/manifest.rs
- crates/execution/src/execution/composite_exec.rs
- crates/execution/src/execution/tests/composite_working_directory.rs
- crates/execution/src/execution/tests/README.md
- crates/execution/tests/composite_runner_context_test.rs
- crates/execution/tests/composite_runner_temp_test.rs
- crates/toolu-runner/tests/fixtures/local_actions/composite-81-*/action.yml
- README.md; docs/test-coverage.md

## Verification

S1 tests literal/expression/absolute/space/absent/empty cwd using pwd and marker
files. S2 tests repeated nested invocations and subsequent caller defaults.
S3 proves missing cwd/malformed expressions prevent execution but run cleanup,
including continue-on-error outcome/conclusion. Full gate mandatory. Record host,
backend and external lanes as verified only after actual runs. Final ledger
--verify plus pre-push review and verdict must be green. The brief authorizes
commit/push/PR and babysit, but no merge or other-worktree mutation.

## Review

Plan-review approved: all AC refs mapped; paths include replay source, fixtures,
production and docs; gate covers whole tree. Jev alignment 0.81; deterministic
review confirms runnable checks and dependencies.

## Deviations

Native macOS build scripts stalled in `_dyld_start` (sampled before main). Docker
cannot bind-mount this /Volumes path, so validation uses a dedicated
`toolu-81-validation` container with a copied worktree snapshot and its own Git
index. Before each verification, copy all edited/added paths from this worktree,
then stage them in the container index so guardrails see them. Container image
`toolu-102-gate-tools:local` supplies Rust 1.94.1, jq and ast-grep. The exact
repository gate remains unchanged. Plan-review reapproved this environment-only
change; Jev chose snapshot validation with confidence 0.99.

## Execution outcome

Six focused regressions passed after failing on the original code. The local full
gate passed fmt/clippy/no-allow/guardrails, then integration-test linking exhausted
Docker storage. Native macOS build scripts stall before main in dyld.

On resume, the orchestrator explicitly authorized committing and opening the PR
with GitHub CI as the full gate when local dyld/disk prevents completion. The
canonical checks above run in CI; local full-gate completion is not claimed.
The existing ci workflow runs every gate layer on Linux and clippy/tests on macOS.
No CI layer is changed. Ready remains conditional on CI passing and the babysit
success audit (zero unresolved threads and an approved zero-finding verdict).
