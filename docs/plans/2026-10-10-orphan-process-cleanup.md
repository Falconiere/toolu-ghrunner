# Orphan process cleanup

**Date:** 2026-10-10   **Status:** Approved   **Spec:** docs/specs/2026-10-10-orphan-process-cleanup-design.md   **Topic:** Issue #89, per-job `RUNNER_TRACKING_ID` and job-end orphan termination

## Evidence and approach

The pinned upstream `JobExtension.cs:586-600` and `880-925` set
`RUNNER_TRACKING_ID=github_<guid>` on the worker process environment, gated by
`process.clean` (default true). At "Complete job" they SIGKILL every other
process whose environment carries that id. Linux reads `/proc/<pid>/environ`
and macOS reads `ps e`.

toolu spawns host step children in exactly two places, `handlers/script.rs:229`
and `handlers/node_exec.rs:136`. Both finalize the child environment through
`step_process_env::apply`. The runner's own environment reaches steps only
through `context::safe_process_env_vars`.

Every job outcome that follows the body funnels through
`job_runner/entry.rs::finish_execution`. That covers success, failure, cancel
and body error. `finish_container` runs before it.

`sysinfo 0.37` is already pinned by `config`. It reads the environment on Linux
from `/proc` and on macOS via `KERN_PROCARGS2`, and it exposes `start_time`,
`status` and `kill_with`.

The reused real inputs are:
- the sanitized #68 capture `incoming_contexts_matrix_0.json` and the #73
  capture `job_container_message.json`;
- the `step_process_ci_test.rs` and `post_results_test.rs` replay patterns.

The design is the spawn-time host flag, per the approved spec:
- the inherited value is stripped from the generic fold;
- the value is passed back through when `process.clean=false`;
- there is no snapshot;
- cleanup uses a deadline-bounded re-scan, then SIGKILL, then a wait for exit;
- it runs in `finish_execution` before `JobCompleted`.

## Workstream summary

1. Real-process replay tests and fixtures (red).
2. The tracking id and spawn-flag injection.
3. The sweep module and its wiring into `finish_execution`.
4. The Linux Docker lane.
5. The reference workflow on official runners.
6. Docs and the evidence map.
7. The full gate, then the PR.

## Steps (machine-readable)

```json
[
  {
    "id": "red_tests",
    "title": "Add failing real-process replay tests and flat Node fixtures for tracking id and job-end cleanup (red evidence: the tracking/cleanup tests are run once and recorded failing before implementation; this check proves they compile against the unchanged API)",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-6"
    ],
    "paths": [
      "crates/execution/tests/orphan_cleanup_test.rs",
      "crates/execution/tests/orphan_89_action.yml",
      "crates/execution/tests/orphan_89_main.js",
      "crates/execution/tests/orphan_89_post.js",
      "crates/execution/tests/incoming_contexts_matrix_0.json"
    ],
    "input": "Sanitized GitHub.com capture incoming_contexts_matrix_0.json restricted to __run steps; real Bash scripts that start a stdout-holding child, a bash -c grandchild, a nohup sleeper and a python3 os.setsid sleeper and write their pids; a real job-started hook script; a local Node action whose post spawns a detached sleep and exits 1; injected process.clean and step environment tokens; liveness checked with real `ps -o stat= -p`.",
    "check": "cargo test -p execution --test orphan_cleanup_test --no-run"
  },
  {
    "id": "tracking_env",
    "title": "Per-job ProcessTracking id, process.clean parse, inherited-id strip/passthrough, and tracking_id threaded to every host spawn",
    "ac_refs": [
      "AC-1",
      "AC-4"
    ],
    "depends_on": [
      "red_tests"
    ],
    "paths": [
      "crates/execution/Cargo.toml",
      "Cargo.lock",
      "crates/execution/src/execution.rs",
      "crates/execution/src/execution/orphan_cleanup.rs",
      "crates/execution/src/execution/context.rs",
      "crates/execution/src/execution/context_process.rs",
      "crates/execution/src/execution/step_process_env.rs",
      "crates/execution/src/execution/handlers/script.rs",
      "crates/execution/src/execution/handlers/node_exec.rs",
      "crates/execution/src/execution/steps_runner.rs",
      "crates/execution/src/execution/job_hooks.rs",
      "crates/execution/src/execution/composite_exec.rs",
      "crates/execution/src/execution/composite_shell.rs",
      "crates/execution/src/execution/node_stage.rs",
      "crates/execution/src/execution/job_runner/entry.rs",
      "crates/execution/src/docker/container_exec.rs",
      "crates/execution/src/docker/job_container.rs",
      "crates/execution/src/docker/action_container.rs",
      "crates/execution/src/config.rs",
      "crates/execution/src/execution/tests/orphan_cleanup.rs",
      "crates/execution/tests/shell_templates_test.rs",
      "crates/execution/tests/orphan_cleanup_test.rs",
      "crates/execution/tests/orphan_89_action.yml",
      "crates/execution/tests/orphan_89_main.js",
      "crates/execution/tests/orphan_89_post.js"
    ],
    "input": "Actual stdout markers from real Bash, Node and hook children in the captured replay: github_<uuid v4>, equal within a job, different across jobs; a test-binary re-exec with RUNNER_TRACKING_ID=github_outer89 sees the job id, or github_outer89 when process.clean=false; step env '' kept verbatim. Parser table: False and ' FALSE ' disable; 0, no and garbage enable.",
    "check": "cargo test -p execution --test orphan_cleanup_test tracking && cargo test -p execution --lib orphan_cleanup::tests::process_clean && cargo test -p execution --test shell_templates_test"
  },
  {
    "id": "sweep",
    "title": "Deadline-bounded re-scan/verify/SIGKILL/wait sweep with job-level logs, wired into finish_execution before JobCompleted",
    "ac_refs": [
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5",
      "AC-6"
    ],
    "depends_on": [
      "tracking_env"
    ],
    "paths": [
      "Cargo.lock",
      "crates/execution/Cargo.toml",
      "crates/execution/src/config.rs",
      "crates/execution/src/docker/action_container.rs",
      "crates/execution/src/docker/container_exec.rs",
      "crates/execution/src/docker/job_container.rs",
      "crates/execution/src/execution.rs",
      "crates/execution/src/execution/composite_exec.rs",
      "crates/execution/src/execution/composite_shell.rs",
      "crates/execution/src/execution/context.rs",
      "crates/execution/src/execution/context_process.rs",
      "crates/execution/src/execution/handlers/node_exec.rs",
      "crates/execution/src/execution/handlers/script.rs",
      "crates/execution/src/execution/job_hooks.rs",
      "crates/execution/src/execution/job_runner/entry.rs",
      "crates/execution/src/execution/node_stage.rs",
      "crates/execution/src/execution/orphan_cleanup.rs",
      "crates/execution/src/execution/step_process_env.rs",
      "crates/execution/src/execution/steps_runner.rs",
      "crates/execution/src/execution/tests/orphan_cleanup.rs",
      "crates/execution/tests/orphan_89_action.yml",
      "crates/execution/tests/orphan_89_main.js",
      "crates/execution/tests/orphan_89_post.js",
      "crates/execution/tests/orphan_cleanup_test.rs",
      "crates/execution/tests/shell_templates_test.rs"
    ],
    "input": "Same captured replay: four orphan kinds dead after success, exit 1 and cancel, with a Terminate line per pid; concurrent job B's detached process (A's step blocks until the test has seen B's pid file) and the unrelated sleep survive A's sweep; '' opt-out survives, upper-cased id killed; hook-exit-1 and failing Node post orphans killed with Failure; cancel and shutdown fired on the cleanup header still kill. Unit tests on real processes: exec env -i between scan and kill means no signal and the process is alive; an unreaped zombie is skipped; a passed deadline gives timed_out.",
    "check": "cargo test -p execution --test orphan_cleanup_test && cargo test -p execution --lib orphan_cleanup"
  },
  {
    "id": "container_lane",
    "title": "Linux real-Docker job-container replay: container step has no RUNNER_TRACKING_ID and the host sweep leaves the container alone",
    "ac_refs": [
      "AC-7"
    ],
    "depends_on": [
      "sweep"
    ],
    "paths": [
      "crates/execution/tests/orphan_cleanup_container_test.rs",
      "crates/toolu-runner/tests/fixtures/job_container_message.json",
      "crates/execution/src/execution/orphan_cleanup.rs",
      "crates/execution/src/execution/job_runner/entry.rs"
    ],
    "input": "Sanitized #73 job_container_message.json replayed against the local Docker 29 daemon; the container step prints TRACK89|container|unset and the job completes Success with the cleanup header logged.",
    "check": "mkdir -p target/orphan89-ctr && TOOLU_CONTAINER_TEST_ROOT=$PWD/target/orphan89-ctr cargo test -p execution --test orphan_cleanup_container_test -- --ignored"
  },
  {
    "id": "reference_workflow",
    "title": "Reference workflow running the same detach patterns on official GitHub-hosted runners plus a gated toolu lane",
    "ac_refs": [
      "AC-8"
    ],
    "depends_on": [
      "sweep"
    ],
    "paths": [
      ".github/workflows/orphan-cleanup-89.yml",
      ".github/actionlint.yaml"
    ],
    "input": "GitHub-hosted ubuntu-24.04 and macos-15 run the official runner pinned by the platform; step logs print the RUNNER_TRACKING_ID shape and detached pids, and the job's Complete-job log shows 'Terminate orphan process' lines fetched with gh run view --log. Post-push (babysit phase): `gh run view <id> --log` for both reference jobs must contain 'Terminate orphan process: pid' for each detached pid; the run URL is then recorded in docs/test-coverage.md. Without that observed log the lane is recorded unverified, never passing.",
    "check": "python3 -c \"import yaml,sys; w=open('.github/workflows/orphan-cleanup-89.yml').read(); d=yaml.safe_load(w); t=w+open('scripts/test/orphan_cleanup_probe.sh').read(); need=['ubuntu-24.04','macos-15','RUNNER_TRACKING_ID','nohup','setsid','run_toolu']; miss=[n for n in need if n not in t]; sys.exit(1 if miss or not d.get('jobs') else 0)\""
  },
  {
    "id": "docs",
    "title": "README, test-coverage evidence map, architecture, AGENTS and module READMEs",
    "ac_refs": [
      "AC-8"
    ],
    "depends_on": [
      "sweep",
      "container_lane",
      "reference_workflow"
    ],
    "paths": [
      "README.md",
      "docs/test-coverage.md",
      "docs/architecture.md",
      "AGENTS.md",
      "crates/execution/src/execution/README.md",
      "crates/execution/src/execution/tests/README.md"
    ],
    "input": "Measured test names, commands, platforms and run links from the steps above; macOS/permission/sudo/env -i/boot-PID-1 limits stated, not claimed.",
    "check": "python3 -c \"import re,sys; t=open('docs/test-coverage.md').read(); m=re.search(r'## Issue #89.*?(?=\\n## |\\Z)', t, re.S); sec=m.group(0) if m else ''; need=['AC-%d'%i for i in range(1,9)]+['unverified','orphan_cleanup_test','orphan_cleanup_container_test','macOS']; miss=[n for n in need if n not in sec]; print(miss); sys.exit(1 if miss else 0)\" && grep -q 'RUNNER_TRACKING_ID' README.md && grep -q 'orphan_cleanup' AGENTS.md docs/architecture.md crates/execution/src/execution/README.md crates/execution/src/execution/tests/README.md && git diff --check"
  },
  {
    "id": "gate",
    "title": "Full repository gate",
    "ac_refs": [
      "AC-8"
    ],
    "depends_on": [
      "docs"
    ],
    "check": "./tools/check.sh all"
  }
]
```

## Critical files

- **Create:**
  - `crates/execution/src/execution/orphan_cleanup.rs`
  - `crates/execution/src/execution/context_process.rs`
  - `crates/execution/src/execution/tests/orphan_cleanup.rs`
  - `crates/execution/tests/orphan_cleanup_test.rs`
  - `crates/execution/tests/orphan_cleanup_container_test.rs`
  - `crates/execution/tests/orphan_89_{action.yml,main.js,post.js}`
  - `.github/workflows/orphan-cleanup-89.yml`
- **Modify:**
  - `crates/execution/Cargo.toml`, `Cargo.lock`
  - `crates/execution/src/execution.rs`
  - `context.rs`, `step_process_env.rs`
  - `handlers/script.rs`, `handlers/node_exec.rs`
  - `steps_runner.rs`, `job_hooks.rs`, `composite_exec.rs`,
    `composite_shell.rs`, `node_stage.rs`
  - `job_runner/entry.rs`
  - `docker/{container_exec,job_container,action_container}.rs`
  - `crates/execution/tests/shell_templates_test.rs`
  - the docs listed in the `docs` step

## Verification

**End to end.** The captured GitHub job replays through the production
`Runner::execute_job` → `run_job` → `finish_execution` path, with real Bash,
Node, python3 and `ps`. Orphans must be observed dead or zombie after the event
stream closes, while opted-out, concurrent and unrelated processes must be
observed alive. Unit tests call the sweep's internal two-phase API on real
processes only, to cover the PID-reuse stand-in, the zombie case and the
deadline.

**Platforms.**
- Linux is verified locally and by CI `ubuntu-latest`.
- macOS is verified by CI `macos-14`, which runs `cargo test --workspace`.
- The Docker lane runs locally on Linux.
- The official-runner reference comes from the workflow's GitHub-hosted runs.
- The toolu self-hosted and GHES lanes are recorded as **unverified** unless a
  runner exists.

**Docs.** They are synchronized in the `docs` step. The `gate` step must exit
0 without suppressions.

**Delivery.** The order is:
1. A scoped commit.
2. `plan-ledger.js run <plan> --verify`.
3. `toolu-review:review`, writing the version-2 push-review state; the push
   hook enforces this.
4. `verdict.js status` reporting `overall: ready`.
5. Push, then create and verify the PR against `main`.
6. `report pr-open`, then `report babysit` and `/pr-babysit:babysit`.

During babysit, the reference workflow run triggered by the push is read with
`gh run view --log`. Its official-runner `Terminate orphan process` lines are
linked in `docs/test-coverage.md` in a follow-up commit; if the lines are
absent, the lane is recorded as unverified.

## Deviations

- `container_lane` check: the Docker replay requires `TOOLU_CONTAINER_TEST_ROOT`
  (a Docker-visible temp root, same contract as `job_container_ci_test.rs`), so
  the check now creates `target/orphan89-ctr` and exports it.
- `reference_workflow`: pre-push review asked to de-duplicate the two jobs'
  inline orphan script, so it moved to `scripts/test/orphan_cleanup_probe.sh`;
  the check now reads the workflow plus that script.

## Review record

- Plan review (self-review plus a Jev step/AC alignment check): the AC
  coverage script found 8/8 ACs mapped, no dangling refs, and dependencies in
  order.
- Findings were fixed before approval: the reference-workflow check was a YAML
  parse only, the docs check only grepped keywords, the red step's
  compile-only check needed an explicit red-evidence note, the `sweep` paths
  were under-declared, and the AC-3 pid-file ordering was missing.
