# Docker container actions implementation

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/specs/2026-09-25-docker-actions-design.md   **Topic:** #75

## Evidence and approach

The approved spec follows ContainerActionHandler at cab9d1c. Existing code exposes
ContainerCommand, JobContainer::network, PathTranslator, StepBounds, command
handlers, captured #73 job fixtures and scoped PostStepQueue. Use these contracts
and retain Linux-only dispatch. No service orchestration (#74) is added here.

## Workstream summary

Owned Docker runtime → production dispatch/stages → documentation/evidence →
paired live validation → full gate and authorized PR delivery.

## Steps (machine-readable)

```json
[
  {
    "id": "runtime",
    "title": "Implement owned Docker image preparation and stage execution with real-daemon regression tests",
    "ac_refs": [
      "AC-2",
      "AC-5"
    ],
    "input": "Pinned Alpine image, real Dockerfile probe, invalid build and cancellation against local Linux daemon",
    "check": "bash scripts/test/docker_actions_linux.sh runtime",
    "paths": [
      "crates/execution",
      "Cargo.lock",
      ".github/actions/docker-action-probe",
      "scripts/test/docker_actions_linux.sh"
    ]
  },
  {
    "id": "dispatch",
    "title": "Wire manifests, registry references, action inputs and pre/main/post dispatch",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5",
      "AC-6"
    ],
    "depends_on": [
      "runtime"
    ],
    "input": "Sanitized acquired #73 envelope with documented Docker reference variations and committed Docker action scripts",
    "check": "bash scripts/test/docker_actions_linux.sh replay",
    "paths": [
      "crates",
      "Cargo.lock",
      ".github/actions/docker-action-probe",
      "scripts/test/docker_actions_linux.sh"
    ]
  },
  {
    "id": "docs",
    "title": "Commit behavior guide and exact scenario evidence mapping",
    "ac_refs": [
      "AC-7"
    ],
    "depends_on": [
      "dispatch"
    ],
    "input": "Issue 75 criteria and real test results",
    "check": "git diff --check && test -s docs/test-coverage.md && test -s .github/workflows/docker-actions-live.yml",
    "paths": [
      "README.md",
      "docs",
      ".github",
      "crates/execution",
      "scripts/test/docker_actions_evidence_check.py"
    ]
  },
  {
    "id": "gate",
    "title": "Run complete repository gate and final committed-diff review before authorized delivery",
    "ac_refs": [
      "AC-7"
    ],
    "depends_on": [
      "docs"
    ],
    "input": "Final branch diff and entire workspace",
    "check": "TOOLU_DOCKER_ACTIONS_TEST_IMAGE=toolu-72-gate-tools:local bash scripts/test/docker_actions_linux.sh gate"
  }
]
```

## Critical files

- crates/execution/src/docker/action_container.rs, action_image.rs, action_mounts.rs
- crates/execution/src/docker.rs and docker/README.md
- crates/execution/src/execution/docker_action.rs, docker_stage.rs
- crates/execution/src/execution/action_exec.rs, action_support.rs, post_drain.rs,
  step_naming.rs, actions/manifest.rs, actions/resolver.rs and module declarations
- crates/execution/src/docker/tests/action_container.rs and actions/tests/manifest_docker.rs
- crates/execution/tests/docker_action_test.rs and docker_action_linux_test.rs
- .github/actions/docker-action-probe/* and .github/workflows/docker-actions-live.yml
- scripts/test/docker_actions_linux.sh
- README.md, docs/architecture.md, docs/test-coverage.md and module README tables

## Verification

Use test-first real tools: verify exact argv/env/commands/state/annotation results,
cache image identity, Linux network connectivity, cancellation after start, timeout,
invalid image/build/entrypoint and resource cleanup. macOS verifies rejection.
No missing/ignored lane is a pass. External live/reference/GHES lanes remain
explicitly unverified when infrastructure is unavailable; the orchestrator has
authorized gate/Docker replay followed by PR delivery and CI babysitting.
Document every deliberate captured-envelope variation separately from acquisition.

Delivery is authorized only for this branch. Fetch/rebase before execution and
before push if main moved; rerun the complete gate. Review the committed diff with
toolu-review, run the final ledger --verify and verdict status. PR body starts
with Closes Falconiere/toolu-ghrunner#75 and Part of Falconiere/toolu-ghrunner#67.
Report pr-open/babysit and invoke the authorized babysit skill; report ready only
at strict success. Never merge.

## Plan review

Approved: AC references, dependency ordering, real-input checks, failure cases,
documentation and authorized delivery were checked. Jev alignment 0.78; exact
coverage is checked by pl_check_ac_refs. No mandatory verification is waived.

## Execution authorization update

The orchestrator confirmed the supported test routes: ./tools/check.sh all on
the host and focused package/replay tests inside Docker. Bare host cargo test
remains denied. Execution resumed with these routes; no deny-rule change is
needed. CI/ci-macos may supply authoritative host evidence if local Node stalls.

## Deviations

The orchestrator's resume instruction authorizes gate/Docker replay followed by
commit/push/PR and babysit. Paired live validation requires the new workflow to
exist on the remote branch, so it cannot be a pre-push ledger dependency. The
required local ledger now ends with documentation and the full gate; external
live/reference/GHES results remain explicitly unverified until run. This is a
delivery sequence correction, not a parity claim. Runtime and dispatch edits
proceed independently against the agreed interface; their checks run in order.

The host full gate passed fmt/clippy/guardrails and reached workspace tests, then
`sample` showed the next binary stopped in `_dyld_start` before its harness.
Under the explicit orchestrator instruction, the gate ledger uses the unchanged
`./tools/check.sh all` inside the Linux carrier (with jq and ast-grep installed);
no layer is omitted. CI/ci-macos still decide delivery readiness.
