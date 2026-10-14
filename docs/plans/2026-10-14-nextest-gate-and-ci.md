# Run the test suite with cargo-nextest in the gate and CI — Plan

**Date:** 2026-10-14   **Status:** Approved   **Spec:** docs/specs/2026-10-14-nextest-gate-and-ci-design.md   **Topic:** issue #118 — nextest in gate, CI and release verify

## Evidence and approach

Inspected `tools/check.sh` `_check_rust`, `ci.yml` (`ci` line ~69, `ci-macos`
~137), `release.yml` `verify` (line 42), `scripts/test/ci_workflow_test.sh`
(macOS assertion line 99, job-scoped `want_ci`/`want_macos`),
`scripts/test/check_script_test.sh`, `.config/nextest.toml`, `bacon.toml`.
Baseline measured: `cargo test --workspace` 1438 passed / 72 ignored, 5m25s;
nextest 1437 passed / 72 skipped, 2m37s. Edit the gate and the three workflow
sites identically, then pin them in the two shell tests.

## Workstream summary

gate script → workflows → shell tests → docs → full gate.

## Steps (machine-readable)

```json
[
  {
    "id": "s1-gate-script",
    "title": "tools/check.sh runs nextest then doctests; fails clearly without cargo-nextest",
    "check": "bash scripts/test/check_script_test.sh && grep -q 'cargo nextest run --workspace' tools/check.sh && grep -q 'cargo test --workspace --doc' tools/check.sh",
    "ac_refs": ["AC-1"],
    "paths": ["tools/check.sh", "scripts/test/check_script_test.sh"],
    "input": "real tools/check.sh"
  },
  {
    "id": "s2-workflows",
    "title": "ci, ci-macos and release verify install nextest and run nextest + doctests",
    "check": "bash scripts/test/ci_workflow_test.sh && test \"$(grep -c 'cargo nextest run --workspace' .github/workflows/ci.yml)\" = 2 && grep -q 'cargo nextest run --workspace' .github/workflows/release.yml",
    "ac_refs": ["AC-1"],
    "depends_on": ["s1-gate-script"],
    "paths": [".github/workflows/ci.yml", ".github/workflows/release.yml", "scripts/test/ci_workflow_test.sh"],
    "input": "real workflow files"
  },
  {
    "id": "s3-workflow-test-negative",
    "title": "ci_workflow_test fails on a workflow copy with the nextest step removed",
    "check": "tmp=$(mktemp) && sed 's/cargo nextest run --workspace/cargo test --workspace/' .github/workflows/ci.yml > \"$tmp\" && ! CI_WORKFLOW_FILE=\"$tmp\" bash scripts/test/ci_workflow_test.sh >/dev/null 2>&1",
    "ac_refs": ["AC-1"],
    "depends_on": ["s2-workflows"],
    "paths": [".github/workflows/ci.yml", "scripts/test/ci_workflow_test.sh"],
    "input": "mutated copy of ci.yml"
  },
  {
    "id": "s4-docs",
    "title": "README and AGENTS describe the nextest gate",
    "check": "grep -q 'cargo nextest run --workspace' README.md && grep -q 'cargo nextest run --workspace' AGENTS.md",
    "ac_refs": ["AC-5"],
    "depends_on": ["s1-gate-script"],
    "paths": ["README.md", "AGENTS.md"],
    "input": "README.md, AGENTS.md"
  },
  {
    "id": "s5-suite-count",
    "title": "full gate passes; nextest runs the same 1437 tests (+1 doctest) as cargo test did",
    "check": "./tools/check.sh all 2>&1 | tee /dev/stderr | grep -q '1437 tests run: 1437 passed'",
    "ac_refs": ["AC-2", "AC-4"],
    "depends_on": ["s1-gate-script"],
    "paths": ["Cargo.toml", "crates"],
    "input": "whole workspace"
  }
]
```

## Critical files

`tools/check.sh`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`,
`scripts/test/ci_workflow_test.sh`, `scripts/test/check_script_test.sh`,
`README.md`, `AGENTS.md`.

## Verification

`./tools/check.sh all` (AC-4); step checks above; CI step timings recorded in
the PR (AC-3) after the first run; negative workflow mutation (s3).
