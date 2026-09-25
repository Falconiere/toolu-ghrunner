# Step process CI flags — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** Issue #72, `CI` and `GITHUB_ACTIONS` process environment

## Problem

Step children currently see no guaranteed `CI` or `GITHUB_ACTIONS` value. Tools can enter interactive or developer modes in a GitHub Actions job. The job-container startup code also forces `CI=true`, losing a supplied value. The pinned official runner forces `GITHUB_ACTIONS=true` and defaults `CI=true` only when the child and runner environments lack `CI`.

## Non-Goals

1. Implement Docker actions (`runs.using: docker` and `docker://`), owned by open #75. No Docker-action process exists in the current dispatcher. Its half of 72-S3 stays unverified until that handler exists; it must use the same process rule.
2. Change expression-context `env.CI`, wire payloads, service variables, job hooks, or runner configuration. The contract concerns actual step processes.
3. Claim GitHub.com, GHES, Linux, macOS, or pinned-reference parity from a local replay alone. Record each lane's real evidence or its unverified reason.

## Architecture

Finalize a child environment after normal job, workflow, step, action, file-command, and runner-environment precedence has been applied. The shared process rule overwrites `GITHUB_ACTIONS` with the literal `true`. It supplies `CI=true` only when the process map has no `CI` and the runner has no `CI`. Existing values, including `false` and the empty string, remain byte-for-byte unchanged. Use this one rule at host shell, Node, composite shell, and Docker job-container exec boundaries; it covers Node pre/main/post and nested composite invocations without relying on each builder to remember the defaults. Read runner `CI` through `execution::config`, consistent with the crate's env rule.

Resolve an action's step `environment` token once at `execute_action`, push it as an `ExecutionContext` temporary overlay while dispatching Node or composite execution, and pop it on every result. `build_node_env` and `composite_env` already read the resulting `visible_env`; this makes the outer composite step env visible to nested processes without persisting it to later job steps. A queued Node `PostStep` snapshots the merged active step overlays and reapplies them during post drain, retaining both its own and inherited composite values (including empty) even if later job env changes. Persistent job env is read live during post, so an unrelated later `GITHUB_ENV` value still takes effect. The captured-job probes showed this missing path for Node and nested composite stages.

A composite `uses:` step already renders its own `env` with composite expression rules. Put those rendered literals into the synthetic `ActionStep.environment` token before `execute_action`; its single overlay then reaches the nested action and its deferred Node post. Keeping a separate outer overlay would leave the synthetic step's post snapshot empty.

At job-container creation, calculate and retain a base `CI` value: `ContainerSpec.env.CI`, otherwise runner `CI`, otherwise `true`. Force `GITHUB_ACTIONS=true` in the base container too. Every later Docker exec applies the same process rule after the per-step map is assembled, using the retained base `CI` when no workflow/job/step `CI` is present. Thus a step can override the container base, while `GITHUB_ACTIONS` cannot. Host runner `CI` is relevant to container jobs as required by 72-S1, even though current Docker startup only checks the container spec.

This boundary rule matches [ProcessInvoker at `cab9d1c`](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Sdk/ProcessInvoker.cs#L282-L289) and the pinned [DockerCommandManager](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/Container/DockerCommandManager.cs), while keeping the existing filtered runner env path that excludes `TOOLU_RUNNER_*` credentials.

## Interfaces / Schema

- An internal `step_process_env` helper accepts a mutable `HashMap<String, String>` and an optional container-base `CI` fallback. It forces `GITHUB_ACTIONS=true`; only when the map lacks `CI`, it uses the container fallback, then runner `CI` through `config::var`, then `true`.
- Host `ScriptHandler`, `execute_node_action`, `run_shell_script`, and `JobContainer::exec_config` use the helper before launch. `JobContainer` retains its effective base `CI` and passes it to `exec_config`; Docker `create_body` uses the same rule on the initial `ContainerSpec.env` map.
- `execute_action` owns one `resolve_step_env(step, ctx, eval_ctx)` result, pushes it through `ExecutionContext::push_step_env` for action dispatch, then pops it after success or error. `build_post_step` uses `ExecutionContext::snapshot_step_env` to store only the merged active overlays in the internal `PostStep`; `post_drain::run_one_post` pushes and pops that snapshot around the post stage. No wire shape or config key changes.

## Failure modes and edge cases

- An absent `CI` becomes `true`; runner `CI=false` or `CI=''` remains so when no workflow/step value exists. Workflow/job `CI` wins over a container declaration and runner `CI`; a step `CI` wins over the job value, including an empty string.
- `GITHUB_ACTIONS=false` or empty from the runner, workflow, job, step, action manifest, or container declaration becomes `true` in the launched child. The process output, not merely a constructed map, is the oracle.
- A later `GITHUB_ENV` write of `CI` affects later stages/steps by normal job-env precedence; a step-only value stays local. Node pre/main/post and nested composite action boundaries receive the same rule.
- A failed expression while resolving an action step env fails the step visibly, as normal; it must not silently fall back to `CI=true`.
- Container startup/exec failure and unavailable Docker remain failures of their existing lifecycle. macOS does not run Linux job-container steps. Docker actions stay unsupported under #75 and cannot be counted as successful coverage.

## Acceptance criteria

- **AC-1:** Given a captured GitHub job replay with absent `CI`, runner `CI=false`, and workflow/job/step overrides including `CI=''`, real script and Node child output reports the expected highest-precedence value; only fully absent `CI` reports `true`.
- **AC-2:** Given runner, step, and container values attempting to set `GITHUB_ACTIONS=false` or empty, every applicable real child prints `GITHUB_ACTIONS=true`.
- **AC-3:** Given real shell, Node pre/main/post, and nested composite actions in a captured-job replay, each process prints the same `CI`/`GITHUB_ACTIONS` policy, with no stage omitted.
- **AC-4:** Given a Linux job-container message and a real Docker daemon, a shell, Node stage, and nested composite process inside the image print the required values, preserve `CI` overrides, and report their container identity; no host fallback occurs.
- **AC-5:** The repository gate `./tools/check.sh all` passes and the user documentation and `docs/test-coverage.md` state the measured support and unverified lanes.

## Acceptance evidence

| AC / issue scenario | Real input and exact observable result | Runnable proof and applicability |
| --- | --- | --- |
| AC-1 / 72-S1 | Sanitized #68 GitHub.com acquisition (`crates/execution/tests/incoming_contexts_matrix_0.json`), with labelled CI token substitutions, executes real Bash/Node probes. Absent everywhere prints `true`; runner `false` prints `false`; workflow/job `job` prints `job`; step `''` prints empty. The normal precedence case step `step` over job `job` over runner `false` prints `step`. | New sibling integration test under `crates/execution/tests/step_process_ci_test.rs`, using `Runner::execute_job` and process stdout markers. Run with isolated runner env (`env -u CI` and `CI=false`) so host values do not depend on the caller shell. macOS and Linux host lanes separately. |
| AC-2 / 72-S2 | Same real processes, plus a Linux job container, see `GITHUB_ACTIONS=true` after both host and step input say `false` or empty; assertions execute inside shell/Node/container, not against the builder map. | Same replay test plus `crates/execution/tests/job_container_ci_test.rs` on Linux with real Docker. |
| AC-3 / 72-S3 host paths | Committed local Node action emits pre/main/post markers, nested composites emit inner shell markers, and nested `uses:` Node posts retain own and inherited empty `CI` after a later `GITHUB_ENV` write. All stage output lines contain exact `CI` and `GITHUB_ACTIONS` pairs. | Same replay test with checked-in local action fixtures and real Bash/Node. A branch workflow defines identical actions for toolu and official runner lanes at equivalent host capability and action revisions; missing live lanes remain unverified. |
| AC-4 / 72-S3 job-container path | Sanitized acquired `jobContainer` payload from #73, preserving token types and IDs, runs the same shell/Node/composite probes under Docker. Each marker includes expected flags and the same non-host container identity; `CI=false` and `CI=''` survive. | New Linux integration test, `cargo test -p execution --test job_container_ci_test` with the existing real-Docker test root. On macOS this lane is not applicable because job containers are Linux-only; Docker actions remain unverified pending #75. |
| AC-5 / gate and docs | Full gate exits 0 without suppressions; README describes step flags and coverage map lists exact test commands, fixture provenance, workflow/run links, platform/backend applicability and unverified reasons. | `./tools/check.sh all`; inspect `README.md` and `docs/test-coverage.md`. Missing live runner, GHES, or pinned-reference evidence is recorded as unverified, never passed. |

For live comparison, use one pinned workflow/action revision and equivalent host capabilities; record the actual reference runner binary revision. GitHub.com job logs supply process output. GHES has the same process rule but needs a real GHES acquisition to count as verified. Capture sanitization must preserve wire structure and types. If the capture or Docker daemon is unavailable, the applicable lane remains unverified.

## Documentation impact

Update `README.md` step-environment behavior and `docs/test-coverage.md` with AC/72-S1–S3 results, commands, provenance, live links, and explicit unsupported/unverified lanes. Update `docs/architecture.md` only if the internal process boundary description needs correction.

## Open Questions

None. The epic brief authorizes resolving design choices from the issue, pinned upstream, and current code without an interactive approval round. #75 owns Docker-action execution; this issue records that dependency without treating its missing process as passing evidence.
