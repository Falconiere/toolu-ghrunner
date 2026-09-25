# Step process CI flags

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/specs/2026-09-25-step-process-ci-flags-design.md   **Topic:** Issue #72

## Evidence and approach

The pinned upstream `ProcessInvoker` forces `GITHUB_ACTIONS=true` and defaults `CI=true` only when neither child nor runner defines it; `DockerCommandManager` applies the analogous container watermark. Current toolu paths are `ScriptHandler`, `execute_node_action`, `composite_shell`, and `JobContainer::exec_config`. `JobContainer::create_body` unconditionally writes `CI=true`, and `build_node_env` passes an empty step map. Reuse the sanitized #68 and #73 acquisitions and existing real Bash/Node/Docker replay patterns. Apply one process-finalization rule at the launch boundaries; retain container base CI and the Node action's resolved step env for posts. Docker actions belong to open #75 and remain explicitly unverified.

## Workstream summary

Build process-level replay probes first, wire the shared policy through host and container launch points, verify the live workflow where infrastructure permits, document the evidence, then pass the full gate and deliver the PR.

## Steps (machine-readable)

```json
[
  {
    "id": "host_probes",
    "title": "Add failing captured-job host probes for CI precedence and all applicable stages",
    "ac_refs": ["AC-1", "AC-2", "AC-3"],
    "paths": ["crates/execution/tests/incoming_contexts_matrix_0.json", "crates/execution/tests/step_process_ci_test.rs", "crates/toolu-runner/tests/fixtures/local_actions/ci-72-*/**", "crates/execution/src/execution/**"],
    "input": "Sanitized #68 acquisition preserving wire IDs/token types; labelled workflow/job/step CI tokens, local Bash and Node pre/main/post and nested composite action probes. Each prints a CI72 stdout marker with the observed CI and GITHUB_ACTIONS values. Cases include absent, false, empty, later GITHUB_ENV, and attempted GITHUB_ACTIONS override.",
    "check": "env -u CI cargo test -p execution --test step_process_ci_test --no-run && CI=false cargo test -p execution --test step_process_ci_test --no-run"
  },
  {
    "id": "host_policy",
    "title": "Implement shared launch-time policy for script, Node, and composite shells",
    "ac_refs": ["AC-1", "AC-2", "AC-3"],
    "depends_on": ["host_probes"],
    "paths": ["crates/execution/src/config.rs", "crates/execution/src/execution.rs", "crates/execution/src/execution/step_process_env.rs", "crates/execution/src/execution/handlers/script.rs", "crates/execution/src/execution/handlers/node_exec.rs", "crates/execution/src/execution/composite_shell.rs", "crates/execution/src/execution/README.md", "crates/execution/guardrails.config.json", "crates/execution/tests/step_process_ci_test.rs", "crates/toolu-runner/tests/fixtures/local_actions/ci-72-*/**"],
    "input": "Actual process output from the captured-job probes: no CI -> true, runner false -> false, step empty -> empty; every GITHUB_ACTIONS override -> true. Verify no private TOOLU_RUNNER_* env leaks.",
    "check": "env -u CI cargo test -p execution --test step_process_ci_test host_script && CI=false cargo test -p execution --test step_process_ci_test host_script"
  },
  {
    "id": "node_step_env",
    "title": "Apply action step environment as a temporary overlay and retain it in Node post",
    "ac_refs": ["AC-1", "AC-2", "AC-3"],
    "depends_on": ["host_policy"],
    "paths": ["crates/execution/src/execution/action_exec.rs", "crates/execution/src/execution/composite_uses.rs", "crates/execution/src/execution/context_scopes.rs", "crates/execution/src/execution/action_support.rs", "crates/execution/src/execution/node_stage.rs", "crates/execution/src/execution/post_drain.rs", "crates/execution/src/execution/step_naming.rs", "crates/execution/src/execution/step_env.rs", "crates/execution/src/execution/composite_env.rs", "crates/execution/tests/step_process_ci_test.rs", "crates/toolu-runner/tests/fixtures/local_actions/ci-72-*/**"],
    "input": "Captured Node action step has CI='' while a later real Bash step writes CI=later to GITHUB_ENV; Node pre/main/post each prints empty CI while later job env stays separate. Captured parent composite step CI='' reaches its nested shells, and nested uses Node posts retain both own and inherited empty CI. A malformed CI expression fails visibly.",
    "check": "env -u CI cargo test -p execution --test step_process_ci_test node && CI=false cargo test -p execution --test step_process_ci_test node && env -u CI cargo test -p execution --test step_process_ci_test composite_shell"
  },
  {
    "id": "container_policy",
    "title": "Preserve base CI at Docker create and apply the policy to each container exec",
    "ac_refs": ["AC-1", "AC-2", "AC-4"],
    "depends_on": ["node_step_env"],
    "paths": ["crates/execution/src/docker/job_container.rs", "crates/execution/src/docker/container_exec.rs", "crates/execution/src/execution/step_process_env.rs", "crates/execution/tests/job_container_ci_test.rs", "crates/toolu-runner/tests/fixtures/job_container_message.json", "crates/toolu-runner/tests/fixtures/local_actions/ci-72-*/**", "scripts/test/step_process_ci_linux.sh"],
    "input": "Sanitized #73 acquired jobContainer message replayed on a real Linux Docker daemon with pinned image, real shell/Node/nested composite probes. Assert identical container identity, CI from declaration/runner/job/step (including empty), forced GITHUB_ACTIONS, and no host fallback. Explicit lane fails when Docker is unavailable.",
    "check": "bash scripts/test/step_process_ci_linux.sh"
  },
  {
    "id": "live_evidence",
    "title": "Compare identical workflow/action revision on toolu and pinned official runner where provisioned",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4"],
    "depends_on": ["container_policy"],
    "paths": [".github/actionlint.yaml", ".github/workflows/step-process-ci-72.yml", "crates/toolu-runner/tests/fixtures/local_actions/ci-72-*/**", "crates/execution/tests/step_process_ci_evidence.json", "scripts/test/step_process_ci_evidence_check.py", "crates/execution/tests/step_process_ci_test.rs", "crates/execution/tests/job_container_ci_test.rs"],
    "input": "Same committed workflow/action SHA on toolu and official cab9d1c runner with equivalent macOS/Linux host capability; exact per-stage output and GitHub job logs. Record actual runner revisions and run links. GHES and absent hosts/credentials are unverified, never passing.",
    "check": "python3 scripts/test/step_process_ci_evidence_check.py crates/execution/tests/step_process_ci_evidence.json"
  },
  {
    "id": "docs",
    "title": "Document supported process behavior and AC/72-S1-S3 evidence with explicit gaps",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4", "AC-5"],
    "depends_on": ["live_evidence"],
    "paths": ["README.md", "docs/test-coverage.md", "docs/specs/2026-09-25-step-process-ci-flags-design.md", "crates/execution/tests/step_process_ci_evidence.json", "scripts/test/step_process_ci_evidence_check.py"],
    "input": "Observed test and gate outputs, fixture provenance, workflow/action SHA, live run links when available, Linux/macOS and GitHub.com/GHES applicability; Docker action half of 72-S3 stays unverified until #75.",
    "check": "python3 scripts/test/step_process_ci_evidence_check.py crates/execution/tests/step_process_ci_evidence.json && git diff --check"
  },
  {
    "id": "delivery",
    "title": "Run full gate and ledger verification, commit, rebase, rerun gate if main moved, push, open PR, and babysit",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4", "AC-5"],
    "depends_on": ["docs"],
    "paths": ["Cargo.toml", "tools/check.sh", "crates/execution/src/**", "crates/execution/tests/**", "crates/toolu-runner/tests/fixtures/**", ".github/actionlint.yaml", ".github/workflows/step-process-ci-72.yml", "README.md", "docs/**", "scripts/test/step_process_ci_evidence_check.py", "scripts/test/step_process_ci_linux.sh"],
    "input": "Complete branch diff and all committed real fixtures; report Docker, GHES, and reference lanes by actual evidence status.",
    "check": "./tools/check.sh all"
  }
]
```

## Critical files

Production: `crates/execution/src/config.rs`, `crates/execution/src/execution/{step_process_env,handlers/script,handlers/node_exec,composite_shell,action_exec,action_support,node_stage,post_drain,step_naming}.rs`, and `crates/execution/src/docker/{job_container,container_exec}.rs`. Tests and fixtures: `crates/execution/tests/{step_process_ci_test,job_container_ci_test}.rs`, sanitized #68/#73 captures, and `crates/toolu-runner/tests/fixtures/local_actions/ci-72-*`. Evidence: `.github/workflows/step-process-ci-72.yml`, `crates/execution/tests/step_process_ci_evidence.json`, `scripts/test/{step_process_ci_evidence_check.py,step_process_ci_linux.sh}`. The Linux wrapper must execute the production test on a Linux host/target with real Docker and fail when it cannot; a Darwin test binary with zero Linux tests cannot count as AC-4 evidence. Docs: `README.md`, `docs/test-coverage.md`.

## Verification

Run failing replay assertions first against actual Bash/Node output, then focused cases after each implementation step. Run Linux Docker job-container tests through a wrapper that enters a Linux build/runtime and fails if Docker or the selected test cases are absent; compare live workflow runs when the infrastructure is available. Missing Docker, toolu/reference runners, or GHES is reported as unverified. Run the evidence checker, `plan-ledger.sh run docs/plans/2026-09-25-step-process-ci-flags.md --verify`, and `./tools/check.sh all`; resolve every warning or failure before push. Rebase on current `origin/main` immediately before execution and again before push if it moves, rerunning the gate then. Commit only issue #72 files, push its branch, open a PR with the brief's two issue lines and verification evidence, and hand off to babysit. Do not merge.

## Deviations

The first captured replay exposed an additional action boundary: the outer composite step's `environment` token was never overlaid for nested processes. The approved spec and `node_step_env` ledger step now use the existing `ExecutionContext::push_step_env`/`pop_step_env` path around all action dispatch, with a saved overlay for Node post. The host policy check was narrowed to scripts; the action-overlay step checks both Node and nested composite paths.

A later nested-Node-post replay exposed that `composite_uses` kept rendered env outside its synthetic `ActionStep`, so the new post snapshot was empty. The nested rendered literals now populate that step's `environment`; `execute_action` owns the one overlay and the post snapshot. The new captured replay failed with `CI=later` in the nested post before this fix and passed afterward.

An inherited-parent CI replay then showed the post snapshot still held only the nested action's own map. `ExecutionContext::snapshot_step_env` now merges active step overlays at registration while leaving persistent job env live. The replay failed with `CI=later` before this correction and passed after it.

The parity workflow uses new `toolu-72-macos` and `toolu-72-linux` custom labels. `.github/actionlint.yaml` now declares them so the committed workflow passes the same static checker as other workflows.

The post-commit full macOS gate exposed a 30-second test-harness timeout in the real Node pre/main/post replay under concurrent worktree builds. A subsequent focused run showed `No space left on device` during Node extraction on the shared host volume. Routing this task's `TMPDIR` to the project volume made all six host tests pass, with concurrent Node replays taking 107 seconds. The harness now allows 240 seconds for real Node startup and workspace teardown, retains the exact process-output assertions, and reports the final eight events if that bound is exceeded.

The Linux Docker replay's Cargo `target` under the Colima-shared macOS home also consumed host disk. Its wrapper now mounts a task-specific named Docker volume as `CARGO_TARGET_DIR`, keeping build artifacts in the Linux VM while the real source and Docker socket remain bind-mounted. Colima does not share the project volume, so using the moved host target through a symlink would fail inside the container.
