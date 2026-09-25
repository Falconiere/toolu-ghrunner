# Authenticated, host-aware action downloads

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/toolu/specs/2026-09-25-authenticated-host-aware-action-downloads-design.md   **Topic:** Server-resolved action archives and safe older-server fallback.

## Evidence and approach

Issue #76 and epic #67 require 76-S1–S4, live service evidence, and a green full gate. The pinned official runner calls Launch `POST /actions/build/{plan}/jobs/{job}/runnerresolve/actions` for Run Service jobs, and V1 `ResolveActionDownloadInfoAsync` resource `27d7f831-88c1-4719-8ca1-6a061dad90eb` for GHES. `LaunchContracts.cs` pins snake_case response fields. Captured `crates/execution/tests/defaults_run_job.json` has a Launch endpoint, job token, API URL, plan/job IDs and service authorization. Existing `execution/actions` owns tarball fetch and single-flight prefetch; `job_runner` owns acquired-message context and masker. Add an acquired-job download context there and keep local actions on their current path. Use the existing streaming/staging/watermark extraction path, with revision-qualified cache identity. The `.github/workflows/action-resolution-live.yml` workflow is the existing live entry; extend it for issue-specific assertions. Server payload capture and GHES/Connect verification must be real; an unavailable lane remains unverified.

## Workstream summary

Pin service contract and fixture → resolve/fetch safely → production replay and live checks → docs and gate → scoped delivery.

## Steps (machine-readable)

```json
[
  {
    "id": "contract",
    "title": "Capture and decode server action download information",
    "ac_refs": ["AC-1", "AC-2"],
    "depends_on": [],
    "paths": ["crates/execution", "crates/shared", "Cargo.toml", "Cargo.lock"],
    "input": "Sanitized acquired Run Service job and captured Launch response for a pinned public action; GHES connection-data/response when accessible",
    "check": "cargo test -p execution --test action_downloads_test -- --nocapture"
  },
  {
    "id": "fetch",
    "title": "Wire scoped credentials, revision cache, fallback, redirect and retry through prefetch and step execution",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4"],
    "depends_on": ["contract"],
    "paths": ["crates/execution", "crates/shared", "Cargo.toml", "Cargo.lock"],
    "input": "Captured job, real pinned action tarball, real loopback HTTP redirect/status/stream responses, moving-ref revision responses",
    "check": "cargo test -p execution --test action_downloads_test -- --nocapture && cargo test -p execution --lib execution::actions"
  },
  {
    "id": "live",
    "title": "Exercise the production job and service path against GitHub.com, GHES and pinned reference where available",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-5"],
    "depends_on": ["fetch"],
    "paths": [".github/workflows/action-resolution-live.yml", "crates/execution", "scripts/test/action_download_evidence_check.py", "docs/test-coverage.md"],
    "input": "Same pinned public/private/internal action revisions on toolu and official runner; GHES/Connect where service is available",
    "check": "python3 scripts/test/action_download_evidence_check.py --live"
  },
  {
    "id": "docs_gate",
    "title": "Synchronize user documentation and run the full repository gate",
    "ac_refs": ["AC-5"],
    "depends_on": ["fetch"],
    "paths": ["."],
    "input": "Final source tree and issue-specific evidence matrix with explicit platform/host applicability",
    "check": "./tools/check.sh all && python3 scripts/test/action_download_evidence_check.py"
  }
]
```

## Critical files

`crates/execution/src/execution/actions/download_info.rs`, `resolver.rs`, `downloader.rs`, `prefetch.rs`, `action_exec.rs`, `job_runner/prepared.rs`, `crates/execution/tests/action_downloads_test.rs`, `.github/workflows/action-resolution-live.yml`, `scripts/test/action_download_evidence_check.py`, `README.md`, `docs/test-coverage.md`.

## Verification

Run each ledger check with its real input and read its exact observation; the `live` check must fail when an applicable lane lacks evidence. Run `bash /Users/falconiere/.codex/plugins/cache/toolu/toolu/6.8.1/hooks/lib/plan-ledger.sh run <plan> --verify`, the local review, and `./tools/check.sh all` against the final committed diff. Re-fetch/rebase and repeat the gate if main moved. Commit only issue #76 files, push the authorized branch, open a `main` PR headed `Closes Falconiere/toolu-ghrunner#76` and `Part of Falconiere/toolu-ghrunner#67`, then hand off to babysit. No skipped or mocked service-success lane counts as acceptance.
