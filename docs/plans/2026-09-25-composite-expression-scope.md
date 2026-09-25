# Composite expression, scope, and failure semantics

**Date:** 2026-09-25   **Status:** Approved   **Spec:** docs/specs/2026-09-25-composite-expression-scope-design.md   **Topic:** Issue #102

## Evidence and approach

The pinned upstream `CompositeActionHandler.RunStepsAsync` merges an ordinary failed inner result, updates its scoped step context, and continues condition evaluation; it breaks on a condition evaluation error. Its `action_yaml.json` defines field-specific roots and functions. Current toolu `composite_exec` returns on failure, `composite_expr` erases valid expressions, `composite_uses` drops a nested post, and `composite_shell` waits without the parent bound. `ExecutionContext` already owns scoped steps and action state, and #68 provides sanitized acquired messages and a production replay path. Extend these existing paths, keep scope restoration explicit, and leave #81's cwd path precedence with #81.

## Workstream summary

Establish real fixtures and failing production-path assertions, then implement expression scope, failure conditions, nested side effects/posts, and whole-step bounds. Compare an identical committed workflow with the pinned reference and toolu on equivalent hosts, update documentation/evidence, and deliver only after the full gate is green.

## Steps (machine-readable)

```json
[
  {
    "id": "fixtures",
    "title": "Commit composite actions and captured-message replay tests with exact S1-S5 assertions",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "paths": [
      "crates/execution/tests/incoming_contexts_matrix_0.json",
      "crates/execution/tests/composite_semantics_test.rs",
      "crates/execution/tests/composite_bounds_test.rs",
      "crates/toolu-runner/tests/fixtures/local_actions/**",
      ".github/workflows/composite-semantics-102.yml"
    ],
    "input": "Sanitized #68 acquisition retaining UUID/contextName and token types; checked-in action.yml, real Bash and Node scripts. Assert hello world/42, exact failure markers, one/one and two/two, file-command values/post order, timeout/cancel no-late markers.",
    "check": "cargo test -p execution --test composite_semantics_test --no-run && cargo test -p execution --test composite_bounds_test --no-run"
  },
  {
    "id": "expressions",
    "title": "Replace composite regex renderer with full evaluator and field policies",
    "ac_refs": [
      "AC-1",
      "AC-3"
    ],
    "depends_on": [
      "fixtures"
    ],
    "paths": [
      "crates/execution/src/execution/composite_expr.rs",
      "crates/execution/src/execution/composite_exec.rs",
      "crates/execution/src/execution/composite_env.rs",
      "crates/execution/src/execution/composite_uses.rs",
      "crates/execution/src/execution/action_support.rs",
      "crates/execution/src/execution/context.rs",
      "crates/execution/src/execution/context_scopes.rs",
      "crates/execution/tests/composite_semantics_test.rs"
    ],
    "input": "Real action.yml fields containing format, contains, bracket access, bad syntax and forbidden secrets root, plus repeated nested inputs/outputs.",
    "check": "cargo test -p execution --test composite_semantics_test expressions_render_functions_brackets_conditions_and_outputs && cargo test -p execution --test composite_semantics_test nested_repeated_names_keep_distinct_inputs_and_outputs"
  },
  {
    "id": "failure_policy",
    "title": "Track inner outcome and conclusion and continue through eligible cleanup",
    "ac_refs": [
      "AC-2",
      "AC-3",
      "AC-5"
    ],
    "depends_on": [
      "expressions"
    ],
    "paths": [
      "crates/execution/src/execution/composite_exec.rs",
      "crates/execution/src/execution/composite_env.rs",
      "crates/execution/src/execution/context.rs",
      "crates/execution/src/execution/composite_uses.rs",
      "crates/execution/tests/composite_semantics_test.rs",
      "crates/toolu-runner/tests/composite_continue_on_error_test.rs"
    ],
    "input": "Real Bash exit 1, malformed script expression, and missing nested action followed by ordinary, failure(), always(); continue-on-error false/true.",
    "check": "cargo test -p execution --test composite_semantics_test conditions && cargo test -p toolu-runner --test composite_continue_on_error_test"
  },
  {
    "id": "side_effects",
    "title": "Propagate file commands, isolate step env and state, and register nested Node posts",
    "ac_refs": [
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "depends_on": [
      "failure_policy"
    ],
    "paths": [
      "crates/execution/src/execution/composite_uses.rs",
      "crates/execution/src/execution/context.rs",
      "crates/execution/src/execution/context_scopes.rs",
      "crates/execution/src/execution/steps_runner.rs",
      "crates/execution/src/execution/step_errors.rs",
      "crates/execution/src/execution/step_env.rs",
      "crates/execution/src/execution/node_stage.rs",
      "crates/execution/src/execution/post_drain.rs",
      "crates/execution/tests/composite_semantics_test.rs",
      "crates/execution/tests/post_results_test.rs"
    ],
    "input": "Real shell writes GITHUB_ENV/PATH/OUTPUT and two Node actions write GITHUB_STATE; later shells and LIFO posts assert exact scope and report IDs.",
    "check": "cargo test -p execution --test composite_semantics_test file_commands_step_env_and_nested_posts_are_scoped && cargo test -p execution --test composite_semantics_test nested_post_if_failure_sees_later_global_job_failure && cargo test -p execution --test post_results_test"
  },
  {
    "id": "bounds",
    "title": "Enforce one parent timeout/cancellation bound across composite subprocesses and nested actions",
    "ac_refs": [
      "AC-5",
      "AC-2"
    ],
    "depends_on": [
      "side_effects"
    ],
    "paths": [
      "crates/execution/src/execution/action_exec.rs",
      "crates/execution/src/execution/composite_exec.rs",
      "crates/execution/src/execution/composite_shell.rs",
      "crates/execution/src/execution/composite_env.rs",
      "crates/execution/src/execution/composite_uses.rs",
      "crates/execution/src/execution/step_timeout.rs",
      "crates/execution/tests/composite_bounds_test.rs",
      "crates/execution/tests/composite_semantics_test.rs",
      "crates/toolu-runner/tests/fixtures/local_actions/composite-102-bounds/action.yml"
    ],
    "input": "Real child records started and PID, sleeps, then would write late; trigger parent timeout and observed-start cancel; assert no late write and child reaped.",
    "check": "cargo test -p execution --test composite_bounds_test && cargo test -p execution --test composite_semantics_test observed_start_cancel_kills_child_and_runs_always_cleanup"
  },
  {
    "id": "cwd_integration",
    "title": "Integrate expression rendering at #81's composite cwd site after its implementation lands on main",
    "ac_refs": [
      "AC-1",
      "AC-5"
    ],
    "depends_on": [
      "bounds"
    ],
    "paths": [
      "crates/execution/src/execution/composite_exec.rs",
      "crates/execution/src/execution/composite_shell.rs",
      "crates/execution/tests/composite_semantics_test.rs",
      "crates/execution/tests/composite_bounds_test.rs",
      "docs/specs/2026-09-25-composite-expression-scope-design.md"
    ],
    "input": "Committed composite action with working-directory expression selecting subdir and real shell writing pwd; verify #81's resolution logic is present after rebase.",
    "check": "cargo test -p execution --test composite_semantics_test working_directory && cargo test -p execution --test composite_bounds_test working_directory",
    "status": "waiting for issue #81 to land on main"
  },
  {
    "id": "reference",
    "title": "Run identical workflow/action revisions on toolu and pinned official runner and verify GitHub reporting",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "depends_on": [
      "cwd_integration"
    ],
    "paths": [
      ".github/workflows/composite-semantics-102.yml",
      "crates/toolu-runner/tests/fixtures/local_actions/**",
      "crates/execution/tests/composite_semantics_evidence.json",
      "scripts/test/composite_semantics_evidence_check.py",
      "docs/test-coverage.md"
    ],
    "input": "Real GitHub.com acquisitions on equivalent macOS and Linux hosts for official cab9d1c and toolu, using identical workflow/action commit. Record run URLs, runner binary hashes, exact assertions, final status, log/annotation parent IDs and post order. GHES applicability and unavailable backend lane are recorded separately as unverified, never as passing.",
    "check": "python3 scripts/test/composite_semantics_evidence_check.py crates/execution/tests/composite_semantics_evidence.json --require-live"
  },
  {
    "id": "docs",
    "title": "Update support description, architecture, and AC/S1-S5 coverage matrix with passing links and unverified lanes",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "depends_on": [
      "reference"
    ],
    "paths": [
      "README.md",
      "docs/architecture.md",
      "docs/test-coverage.md",
      "docs/specs/2026-09-25-composite-expression-scope-design.md",
      "crates/execution/tests/composite_semantics_evidence.json",
      "scripts/test/composite_semantics_evidence_check.py"
    ],
    "input": "Actual local test outputs, git hashes, GitHub run URLs, host and backend matrix; mark any missing reference/Linux/GHES result unverified.",
    "check": "python3 scripts/test/composite_semantics_evidence_check.py crates/execution/tests/composite_semantics_evidence.json && python3 -m unittest discover -s scripts/test -p test_composite_semantics_evidence_check.py && git diff --check"
  },
  {
    "id": "delivery",
    "title": "Run full gate, verify ledger, commit, rebase, rerun gate, push, open PR and hand off to babysit",
    "ac_refs": [
      "AC-1",
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-5"
    ],
    "depends_on": [
      "docs"
    ],
    "paths": [
      "Cargo.toml",
      "tools/check.sh",
      "crates/execution/src/execution/**",
      "crates/execution/tests/**",
      "crates/toolu-runner/tests/**",
      "docs/**",
      "scripts/test/composite_semantics_evidence_check.py",
      ".github/workflows/composite-semantics-102.yml"
    ],
    "input": "All committed real fixtures and production replay checks; branch based on current origin/main.",
    "check": "./tools/check.sh all"
  }
]
```

## Critical files

Production: `crates/execution/src/execution/{composite_expr,composite_exec,composite_env,composite_uses,composite_shell,context,action_exec,action_support,steps_runner,step_timeout}.rs`. Tests: two new `crates/execution/tests/composite_*_test.rs` files, existing captured #68 payloads, local action fixtures, and workflow `.github/workflows/composite-semantics-102.yml`. Evidence and docs: `crates/execution/tests/composite_semantics_evidence.json`, `scripts/test/composite_semantics_evidence_check.py`, `README.md`, `docs/architecture.md`, `docs/test-coverage.md`.

## Verification

Run the named cargo tests against the real shell/Node and sanitized acquisition first, including malformed expressions, hard nested action errors, repeated names, timeout and observed-start cancellation. Compare the same committed action/workflow on the pinned official runner and toolu, preserving outcome/conclusion, reports, post order, and log attribution; label inaccessible Linux/GHES lanes unverified. Run the evidence checker and `./tools/check.sh all` without suppressions. Rebase onto current `origin/main`, rerun the gate if it moved, then commit/push and open a PR whose body begins with the brief's two issue lines. The authorized delivery ends at the babysit `ready` status; merging remains with the orchestrator.

## Deviations and remaining dependencies

The captured-job cases were consolidated into `composite_semantics_test.rs`
and the short-deadline subprocess case into `composite_bounds_test.rs`; the
originally named scope/file-command test files were unnecessary duplicates.
The first full gate passed after those tests and the scoped implementation.
A later regression case found that a nested `post-if: failure()` used stale
composite status after a subsequent job failure; it is now fixed and the final
full gate passed. An observed-start cancellation case also confirms that a
failing `always()` cleanup retains the parent Cancelled conclusion. The live
workflow and evidence checker are included as
repeatable proof, with `--require-live` remaining red until actual runs exist.

Issue #81 is still open and owns the composite working-directory parser and
resolver. The `cwd_integration` step stays pending until its code lands on
main. GitHub-hosted runners can provide current reference behavior, but a run
on the pinned `cab9d1c` binary has not yet been recorded; no hosted result
may be labelled as a pinned-binary pass.
