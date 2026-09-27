# Action metadata — Implementation plan

**Date:** 2026-09-26   **Status:** Approved   **Spec:** docs/specs/2026-09-26-action-metadata-design.md   **Topic:** #85

## Evidence and approach

Preserve acquired ActionStep.name; centralize runtime metadata export and
snapshot/restore across top-level steps, actions, embedded scripts and posts.
Existing Runner replay fixtures and real shell/Node probes prove the production
path. Existing CI provides Linux/macOS workspace tests and real Linux Docker.
The orchestrator waived unavailable GHES/live/reference lanes as delivery blockers;
record these as unverified in coverage docs and the PR.

## Workstream summary

Reproduce missing metadata with captured-message replay, implement scoped export,
extend Docker acceptance, synchronize documentation, verify and deliver.

## Steps (machine-readable)

```json
[
  {
    "id": "metadata",
    "title": "Preserve wire identity and scoped metadata through real action execution",
    "ac_refs": ["AC-1", "AC-2", "AC-3"],
    "check": "tar --no-xattrs --no-acls --exclude=./target --exclude=./.git --exclude=./.claude/tmp --exclude=./.codex/tmp -cf - . | docker cp - issue85-verification:/issue85 && docker exec -w /issue85 issue85-verification cargo test -p execution action_metadata",
    "paths": ["crates", "Cargo.toml", "Cargo.lock", ".github/actions/action-metadata-probe"],
    "input": "Sanitized incoming_contexts_matrix_0.json replay through Runner, real Bash/Node actions; named/generated/repeated names, cleared repo/ref, pre/main/post, nesting and failure restoration"
  },
  {
    "id": "docs-gate",
    "title": "Document scenario evidence and pass the complete repository gate",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4"],
    "depends_on": ["metadata"],
    "check": "docker exec -w /issue85 issue85-verification ./tools/check.sh all && git diff --check",
    "paths": ["crates", "scripts", "tools", "docs", "README.md", ".github", "Cargo.toml", "Cargo.lock", "guardrails.workspace.json"],
    "input": "All real workspace fixtures and tool tests; Linux Docker replay in CI; README and coverage scenario map"
  }
]
```

## Critical files

- crates/shared/src/job_message/step.rs and its ActionStep construction sites.
- crates/execution/src/execution/{action_metadata,context,context_env,steps_runner,action_exec,action_support,composite_exec,composite_env,post_drain,step_naming,docker_action}.rs.
- crates/execution/src/execution/tests/action_metadata.rs and .github/actions/action-metadata-probe/.
- crates/execution/tests/docker_action_linux_test.rs, execution README, root README, docs/test-coverage.md.

## Verification

First observe the new real-process test fail for missing metadata, then pass it.
Run the full gate without suppressions; Linux/macOS CI runs the same replay and
Linux CI runs opt-in Docker production tests. Assert exact marker values and
job conclusion; missing tools fail checks. Keep unavailable backend/reference
lanes explicitly unverified. Check all ledger AC mappings and fresh evidence,
perform local committed-diff review, commit scoped files, fetch/rebase if main
moved and rerun the gate before push. Open a conventional-title PR targeting
main with the required closing/epic lines. Run authorized babysit to strict ready
and report status. Never merge. Poll GitHub sparingly.

## Plan review

Approved after checking each AC mapping, existing files, failure restoration,
real input provenance and delivery authorization. Container proof is explicitly
CI-owned: `TOOLU_CONTAINER_TEST_ROOT=/tmp/toolu-docker-action-tests cargo test
-p execution --test docker_action_linux_test -- --ignored --test-threads=1`.
CI must pass before ready; the local host gate alone does not prove Docker.
Jev identified partial confidence (0.61), resolved by retaining this separate
Linux CI check and its evidence obligation.

## Deviations

The host denies bare cargo test. The orchestrator approved Docker-wrapped focused
tests and the unchanged repository gate. Focused ledger checks copy current
source (without host xattrs) to the task-owned issue85-verification container;
Colima cannot bind this /Volumes worktree. Container uses Rust 1.94.1 with a
dedicated issue85-target volume. Final local gate runs the unchanged ./tools/check.sh all in that Linux container.
The earlier macOS gate remains running; required macOS CI independently verifies
the final head. Plan review accepts this route: every gate layer remains enabled,
and Jev selected it over duplicating the slow host run (confidence 1.0).
Plan-review confirms equivalent checks and unchanged acceptance scope.

The real remote-reference dispatch probe exposed nested with-input evaluation
using parent metadata. The approved scope now explicitly includes temporary
child reference installation for input rendering, with parent restoration on
error. Upstream ActionRunner installs reference metadata before EvaluateStepInputs.
A new probe first failed, then validates this path with actual composite/Node code.
Host quality hooks also required splitting existing oversized functions/impls
in the touched step/composite/context files; their behavior is retained.
