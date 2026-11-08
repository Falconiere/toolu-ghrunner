# execution/

**What belongs here:** the job execution engine itself — context assembly,
the step loop, handler-dispatch glue (composite/action/script env building,
workflow-command dispatch, file commands), job lifecycle (start, hooks,
teardown), and the local OIDC/artifact/cache service plumbing a job's steps
talk to.

**What does NOT belong here:** the bollard Docker wrapper (`crate::docker`)
and Node.js runtime download/caching (`crate::node`) — those are siblings this
module calls into, not part of it. There is no local workflow parser or
orchestrator: GitHub expands workflows and sends the engine a single job.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `action_exec.rs` | `execute_action` | End-to-end `uses:` step execution: resolve → download → parse manifest → run `main`, returning the outcome plus any `post` to register. It still runs `pre` inline only for an action the pre-job phase did not claim (a nested remote action); a composite with embedded posts gets one `PostStep` carrying them. |
| `action_pre.rs` | `phase_pre_target` / `run_phase_pre` | One step's pre-job-phase `pre`: a repository Node action's `pre` or, on Linux, a remote Docker action's `pre-entrypoint`. Quiet resolve, numbered `Pre` row, post registered first as upstream; a job whose container never started runs no stage. |
| `action_metadata.rs` | `ActionMetadata` | Captures/restores runtime action identity and repository/ref, and exports authoritative metadata to child processes. |
| `action_support.rs` | `build_node_env` | Shared action-resolution helpers: node-stage env building, composite input merging, manifest reading, and action-header/log emission. |
| `actions.rs` | (mod decl) | Declares the `actions` sub-module (resolver, downloader, manifest). |
| `artifacts.rs` | (mod decl) | Declares the `artifacts` sub-module (backend + service). |
| `cgroup_join.rs` | `spawn_in_cgroup` | Spawns a step process and, if a per-job cgroup path is set, best-effort moves the child into it so `cpu.max`/`memory.max` apply. |
| `command_dispatch.rs` | `CommandDispatcher` | Consumes a step's stdout `::workflow-command::` lines and applies each to the live `ExecutionContext` (outputs, state, masks, groups, annotations); refuses stdout `set-env`/`add-path` (CVE-2020-15228). Decoded `add-mask` registers the exact whole value and every nonempty trimmed CR/LF line, including short values. |
| `command_parser.rs` | `parse_command` | Parses one stdout line into a `WorkflowCommand` (error/warning/notice/debug/group/set-output/add-mask/save-state/…). |
| `composite_env.rs` | `build_step_env` | Composite-action step-skip evaluation, per-step env building, and file-command path management shared by the composite executor. |
| `composite_exec.rs` | `execute_composite_action` | Runs a composite action's `steps:` sequentially as shell subprocesses or nested `uses:`, threading `GITHUB_OUTPUT`/`ENV`/`PATH` file commands between steps. |
| `composite_expr.rs` | `interpolate_composite_expr` | Full expression evaluation against the invocation snapshot, with field-specific context/function validation. |
| `composite_scope.rs` | `ScopeName` / `CompositeOutputs` | Output-isolation scope identifier and a composite manifest's `outputs:` expression map. |
| `composite_shell.rs` | `run_shell_script` | Adapts a composite `run:` step to the shared `ScriptHandler` shell/template, streaming, timeout and cancellation contract. |
| `composite_uses.rs` | `run_nested_uses_step` | Builds a synthetic `ActionStep` for a composite's nested `uses:` step (resolving a `$/` self-repository ref first via `ExecutionContext::resolve_self_uses`, #241; a `./path` ref is marked `repositoryType: self`, as upstream) and recurses through `action_exec::execute_action`, bounded by `DepthTracker`. |
| `context.rs` | `ExecutionContext` | Mutable per-job execution state: env, per-step outputs/state/conclusions, runtime-owned contexts plus typed incoming server roots (`matrix`/`needs`/`inputs`/`strategy` and future keys), the shared `SecretMasker`, and expression evaluation. Its execution-only workspace root also anchors problem-matcher paths. It owns `$/` self-repository resolution for execution and setup discovery (#241), and the pre-job row counter and flags behind "Initialize containers"/"Pre" row numbering. |
| `context/` | Scoped state helpers | Keeps expression-visible step scopes separate from private action state. |
| `context_env.rs` | `build_step_env` / `build_host_step_env` | `ExecutionContext` env assembly: global + step env + `PATH` additions, the host child's inherited env (runner env minus `TOOLU_RUNNER_*`, overlaid by the #94 network export from `set_network_env`), and the Docker action `PATH`. |
| `context_eval.rs` | `eval_context` | Builds the `EvalContext` snapshot (`github`, `env`, `steps`, `job`, scoped job status) that expressions evaluate against. |
| `context_process.rs` | `process_tracking_env` | `ExecutionContext` accessors for the job's `RUNNER_TRACKING_ID` (issue #89); falls back to the runner's inherited value only when `process.clean=false`. |
| `context_build.rs` | `build_strategy` | Pure helpers for `ExecutionContext`: `runner.debug` detection and the `strategy.*` object, split out to keep `context.rs`'s `impl` blocks small. |
| `context_scopes.rs` | `push_step_env` / `pop_step_env` | Nested `uses:` step env overlays (visible only for that action's stages, snapshotted for deferred posts), the scoped `github.action_path` / `action_status`, and the nested Node post queue (a composite takes its own posts by start mark). |
| `docker_action.rs` | `run_docker_action` / `run_phase_pre` | Linux Docker action image preparation, conditional pre stage and deferred post registration. `run_phase_pre` is the pre-job-phase entry: prepare, register the post first, then run `pre-entrypoint` as a numbered row; the main step then skips both. |
| `docker_prepare.rs` | `prepare` | Discovers remote Docker actions before workflow steps, visiting refs once under the depth limit and resolving enabled nested `$/` refs against their parent. Cancellation interrupts discovery, including fetches. Reports deduplicated Pull rows and Build rows named by upstream's `owner/repo[/path]@ref`, and records images for step reuse. |
| `docker_stage.rs` | `run_docker_stage` | Container argv/env evaluation and normal workflow/file-command dispatch. |
| `depth_tracker.rs` | `DepthTracker` | Tracks composite-action nesting depth and errors past `MAX_COMPOSITE_DEPTH` (10) to prevent infinite recursion. |
| `file_commands.rs` | `FileCommandManager` | Creates/reads/resets a step's `$GITHUB_ENV`/`OUTPUT`/`PATH`/`STATE`/`STEP_SUMMARY` temp files and parses their contents. |
| `handlers.rs` | (mod decl) | Declares the `handlers` sub-module (script/node/node_exec/docker/composite/resolve) and its dispatch order. |
| `job_hooks.rs` | `run_job_hook` | Runs the self-hosted `ACTIONS_RUNNER_HOOK_JOB_STARTED`/`_COMPLETED` scripts around a job; started is a hard gate, completed is best-effort. |
| `container_job.rs` | Job-container lifecycle | Validates the host; reports container start as the numbered "Initialize containers" row after the Docker setup rows and removal as "Stop containers", the last row before "Complete job" (upstream `JobExtension`/`ContainerOperationProvider`). |
| `job_context.rs` | Job context construction | Extracts message variables/context, materializes the event payload and enables `$/` self-repository refs (#241) from `actions_self_repository` + the `job` context's `workflow_repository`/`workflow_sha`. |
| `job_cancellation.rs` | `JobCancellation` | Separate graceful job cancellation from shutdown; share one cleanup deadline and re-evaluate running conditions. |
| `job_environment.rs` | `apply_job_environment` | Evaluates acquired workflow/job env mappings in order, before container setup, using one snapshot per layer. |
| `job_runner.rs` | `run_job` | The job entry point: prepares dirs, starts local services per `ServicesMode`, seeds job env, runs the prepared job body, reports "Stop containers" after the steps and posts and before "Complete job", and returns a `JobTeardown`; `run_job_with_shutdown` empties `_temp` and `tmp` after `entry::run` returns, success or error, while it still holds the event channel open; `job_runner/` contains the prepared body. |
| `job_spec.rs` | `JobSpec` | Acquired `jobOutputs` token plus ordered `defaults.run` mappings (later run mappings replace earlier ones); `evaluate_acquired_outputs` resolves outputs after posts. |
| `job_teardown.rs` | `JobTeardown` | Deferred post-completion work returned by `run_job`: cache staging sweep + GC pass, and joining the workspace-sweep task, run only after the event sender is dropped. |
| `node_stage.rs` | `run_node_stage` / `skip_local_pre` | Runs one Node.js action entrypoint (`pre`/`main`/`post`), rebuilding env per stage and dispatching its stdout workflow commands. A `repositoryType: self` action runs no `pre` and warns, as upstream. |
| `orphan_cleanup.rs` | `ProcessTracking` / `sweep` | Issue #89: per-job `RUNNER_TRACKING_ID` (`process.clean=false` disables it) and the job-end sweep that `SIGKILL`s every other process still carrying it (Linux `/proc`, macOS `KERN_PROCARGS2`) or started during the job inside a registered step process group (`register_group`; covers SIP macOS platform binaries whose environment is hidden), deadline-bounded, never failing the job. Matches exec-time environment snapshots only: `sudo`, other users' processes and processes that leave the group while dropping the id escape (limits in the root README). |
| `oidc.rs` | (mod decl) | Declares the `oidc` sub-module and re-exports `OidcClaims`/`OidcServer`/etc. |
| `post_drain.rs` | `drain_post_steps` | Drains registered posts LIFO, gives each a distinct report ID, keeps draining after errors, and shares the cancellation deadline while reusing the originating action's state. A composite's "Post" row runs its embedded posts last-in first-out, each under its own scope and `post-if`; a missing job container fails a post before dispatch. |
| `service_auth.rs` | `validate_bearer` | Bearer-token validation (constant-time compare) shared by the local OIDC/artifact/cache axum services. |
| `service_endpoints.rs` | `extract_service_urls` / `forward_env` / `ServiceEnv` | Extracts real GitHub service URLs, runtime token, cache-v2 flag, cache mode and (flag-gated) `system.orchestrationId` from the job message and builds the per-step-type (Node / container / `run:`) `ACTIONS_*` env for forwarder mode (`run:` also gets `ACTIONS_ORCHESTRATION_ID`, #241). |
| `service_lifecycle.rs` | `ServiceHandle` | Generic start/shutdown lifecycle for a local axum HTTP service, plus shared 401/500 JSON responses and `Content-Range` parsing. |
| `shadow.rs` | (mod decl) | Declares the `shadow` sub-module; see its own README. |
| `step_attrs.rs` | `evaluate_timeout` / `evaluate_continue_on_error` | Evaluates step `timeout-minutes` / `continue-on-error` tokens at execution time with upstream's number/boolean-only validation and positioned `The template is not valid.` diagnostics. |
| `environment_url.rs` | `evaluate_environment_url` | Evaluates the job's `environment.url` at job end against upstream's no-secrets context; scalar results become strings, a value the masker changes is suppressed (#88). |
| `step_errors.rs` | `report_step_error` / `report_infrastructure_error` | Reports a hard step error (nested failures attributed to the parent step) and emits `RunnerEvent::InfrastructureError` for a categorized `RunnerError::ActionFetch` on the step that hit it (#88). Declared from `steps_runner.rs` via `#[path]`. |
| `step_metadata.rs` | `action_metadata` / `emit_step_metadata` | A reported row's upstream telemetry (`type` / `action_name` / `ref`): `run` + shell, `node{major}`, `composite`, `Dockerfile`, `DockerHub` + image (#88). |
| `step_display.rs` | `name_at_job_start` / `name_at_main` | Upstream's two-phase step display names: job-start message contexts, then live contexts before the condition; prettified pending names, warnings on failure, masked results. |
| `step_env.rs` | `resolve_step_env` | Renders scalar template tokens for step env/script/working-directory/action inputs and applies file-command results back onto the context. |
| `step_process_env.rs` | `apply` | Unconditionally sets `GITHUB_ACTIONS=true` at child launch; fills `CI` only when absent after child, container, and runner precedence; tags host children with the job's `RUNNER_TRACKING_ID` unless a workflow layer set one (container execs are never tagged). |
| `step_naming.rs` | `PostStep` / `PostStepQueue` | The registered-post-step record (a composite's embedded posts ride in `nested`) and its LIFO queue. |
| `step_state.rs` | `StepState` | Per-step recorded outputs/state/outcome/conclusion, and `build_steps_context` for the `steps.*` expression context. |
| `step_summary.rs` | `collect` | Bounded, masked summary snapshots; refuses non-regular summary files; non-fatal error annotations. |
| `step_timeout.rs` | `wait_bounded` | Bounded child-process wait shared by the script and node handlers: races `timeout-minutes` against the job `CancellationToken`. |
| `temp_cleanup.rs` | `empty_temp_dirs` | Empties `_temp` and `tmp` at job start and before the event channel closes: contents only, never following symlinks, continuing past delete errors with one `WARN` per root. Never touches `_tool`. |
| `steps_runner.rs` | `run_steps` | The per-job step loop: condition evaluation, dispatch to action/script execution, continue-on-error, and post-step draining. After a failed container start a step that runs fails before dispatch, never on the host. |
| `workspace_gc.rs` | `gc_workspaces`, `GcPolicy` | Prunes this registration's unleased workspaces: stable ones unused for `stable_gc_hours` (default never) and fresh `<job_id>` ones older than `gc_after_hours`; pre-#90 lease-less dirs use mtime only; never the running job's. |
| `workspace_layout.rs` | `prepare_workspace`, `PreparedWorkspace`, `Repository` | Allocates the job workspace (#90): stable `<work>/<repo>/<repo>` with upstream `workspace.clean`, or fresh `<work>/<job_id>`; holds the lease until dropped. |
| `workspace_lease.rs` | `WorkspaceLease` | `fs2` exclusive lease on a `_PipelineMapping/` lock file; released on drop. |
| `workspace_tracking.rs` | `TrackingConfig`, `tracking_files` | `_PipelineMapping/<owner>/<repo>/PipelineFolder.json` (upstream keys + `lastRunOnUnix`, `dataDir`), atomic writes. |

## Sub-modules

- `actions/` — action resolution, download, and manifest parsing for `uses:` steps.
- `artifacts/` — artifact upload/download service (Azure append-blob backend).
- `handlers/` — the concrete `runs.using` handlers (dispatch itself is in `steps_runner` + `action_exec`).
- `oidc/` — OIDC token server and claims building.
- `shadow/` — shadow-mode (approach C) workspace fingerprinting; records only.
- `tests/` — flat sibling test files for execution module internals.

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.

Problem matcher output support:

| File | Purpose |
| --- | --- |
| `problem_matcher.rs` | Job-wide, case-insensitive registration and atomic replacement/removal. |
| `problem_matcher_model.rs` | Bounded JSON and capture-schema validation, compiled regex definitions. |
| `problem_matcher_state.rs` | Per-process-stream multiline matching, loops and ANSI removal. |
| `problem_matcher_path.rs` | Bounded regular-file loading and workflow-checkout path resolution. |
| `matcher_dispatch.rs` | Matcher command effects and masked annotation emission. |
| `process_dispatch.rs` | Bounded stdout/stderr event bridge shared by shell, Node and composite stages. |
