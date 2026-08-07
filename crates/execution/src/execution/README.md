# execution/

**What belongs here:** the job execution engine itself — context assembly,
the step loop, handler-dispatch glue (composite/action/script env building,
workflow-command dispatch, file commands), job lifecycle (start, hooks,
teardown), and the local OIDC/artifact/cache service plumbing a job's steps
talk to.

**What does NOT belong here:** the bollard Docker wrapper (`crate::docker`),
Node.js runtime download/caching (`crate::node`), and the compiled-in plugin
trait/registry (`crate::plugin`) — those are siblings this module calls into,
not part of it. Workflow YAML parsing/matrix/orchestration lives in the
`workflow/` sub-module, not spread across the step loop.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `action_exec.rs` | `execute_action` | End-to-end `uses:` step execution: resolve → download → parse manifest → run `pre` (if any) → run `main`, returning the outcome plus any `post` to register. |
| `action_support.rs` | `build_node_env` | Shared action-resolution helpers: node-stage env building, composite input merging, manifest reading, and action-header/log emission. |
| `actions.rs` | (mod decl) | Declares the `actions` sub-module (resolver, downloader, manifest). |
| `artifacts.rs` | (mod decl) | Declares the `artifacts` sub-module (backend + service). |
| `cgroup_join.rs` | `spawn_in_cgroup` | Spawns a step process and, if a per-job cgroup path is set, best-effort moves the child into it so `cpu.max`/`memory.max` apply. |
| `command_dispatch.rs` | `CommandDispatcher` | Consumes a step's stdout `::workflow-command::` lines and applies each to the live `ExecutionContext` (outputs, state, masks, groups, annotations); refuses stdout `set-env`/`add-path` (CVE-2020-15228). |
| `command_parser.rs` | `parse_command` | Parses one stdout line into a `WorkflowCommand` (error/warning/notice/debug/group/set-output/add-mask/save-state/…). |
| `composite_env.rs` | `build_step_env` | Composite-action step-skip evaluation, per-step env building, and file-command path management shared by the composite executor. |
| `composite_exec.rs` | `execute_composite_action` | Runs a composite action's `steps:` sequentially as shell subprocesses or nested `uses:`, threading `GITHUB_OUTPUT`/`ENV`/`PATH` file commands between steps. |
| `composite_expr.rs` | `interpolate_composite_expr` | Minimal `${{ }}` interpolation for composite steps (`inputs.*`, `steps.*.outputs.*`, `runner.*`, `env.*`). |
| `composite_scope.rs` | `ScopeName` / `CompositeOutputs` | Output-isolation scope identifier and a composite manifest's `outputs:` expression map. |
| `composite_shell.rs` | `run_shell_script` | Spawns a composite `run:` step's shell script as a subprocess and streams its stdout/stderr as log events. |
| `composite_uses.rs` | `run_nested_uses_step` | Builds a synthetic `ActionStep` for a composite's nested `uses:` step and recurses through `action_exec::execute_action`, bounded by `DepthTracker`. |
| `context.rs` | `ExecutionContext` | Mutable per-job execution state: env, per-step outputs/state/conclusions, `github`/`runner`/`vars`/`secrets`/`matrix`/`strategy` contexts, the shared `SecretMasker`, and expression evaluation. |
| `context_build.rs` | `build_strategy` | Pure helpers for `ExecutionContext`: `runner.debug` detection and the `strategy.*` object, split out to keep `context.rs`'s `impl` blocks small. |
| `depth_tracker.rs` | `DepthTracker` | Tracks composite-action nesting depth and errors past `MAX_COMPOSITE_DEPTH` (10) to prevent infinite recursion. |
| `file_commands.rs` | `FileCommandManager` | Creates/reads/resets a step's `$GITHUB_ENV`/`OUTPUT`/`PATH`/`STATE`/`STEP_SUMMARY` temp files and parses their contents. |
| `handlers.rs` | (mod decl) | Declares the `handlers` sub-module (script/node/node_exec/docker/composite/resolve) and its dispatch order. |
| `job_hooks.rs` | `run_job_hook` | Runs the self-hosted `ACTIONS_RUNNER_HOOK_JOB_STARTED`/`_COMPLETED` scripts around a job; started is a hard gate, completed is best-effort. |
| `job_runner.rs` | `run_job` | The job entry point: prepares dirs, starts local services per `ServicesMode`, seeds job env, runs hooks + the step loop, and returns a `JobTeardown`. |
| `job_spec.rs` | `JobSpec` | Job-level `outputs:` expression map plus merged `defaults.run` (shell/working-directory), and `evaluate_job_outputs` to resolve them post-run. |
| `job_teardown.rs` | `JobTeardown` | Deferred post-completion work returned by `run_job`: cache staging sweep + GC pass, and joining the workspace-sweep task, run only after the event sender is dropped. |
| `node_stage.rs` | `run_node_stage` | Runs one Node.js action entrypoint (`pre`/`main`/`post`), rebuilding env per stage and dispatching its stdout workflow commands. |
| `oidc.rs` | (mod decl) | Declares the `oidc` sub-module and re-exports `OidcClaims`/`OidcServer`/etc. |
| `post_drain.rs` | `drain_post_steps` | Drains the job's `PostStepQueue` LIFO after main steps, evaluating each `post-if` and running the action's `post` entrypoint in its original step scope. |
| `service_auth.rs` | `validate_bearer` | Bearer-token validation (constant-time compare) shared by the local OIDC/artifact/cache axum services. |
| `service_endpoints.rs` | `extract_service_urls` / `forward_env` | Extracts real GitHub service URLs + runtime token from the job message and builds the `ACTIONS_*` env vars for forwarder mode. |
| `service_lifecycle.rs` | `ServiceHandle` | Generic start/shutdown lifecycle for a local axum HTTP service, plus shared 401/500 JSON responses and `Content-Range` parsing. |
| `shadow.rs` | (mod decl) | Declares the `shadow` sub-module; see its own README. |
| `step_env.rs` | `resolve_step_env` | Renders a step's `environment` template token to a string env map and applies file-command results back onto the context. |
| `step_host.rs` | `StepHost` / `DirectHost` | Abstraction for where `run:` steps execute; `DirectHost` spawns a local process (moved into the per-job cgroup when set). |
| `step_naming.rs` | `PostStep` / `PostStepQueue` | The registered-post-step record, its LIFO queue, and `derive_step_name` for step display names. |
| `step_state.rs` | `StepState` | Per-step recorded outputs/state/outcome/conclusion, and `build_steps_context` for the `steps.*` expression context. |
| `step_timeout.rs` | `wait_bounded` | Bounded child-process wait shared by the script and node handlers: races `timeout-minutes` against the job `CancellationToken`. |
| `steps_runner.rs` | `run_steps` | The per-job step loop: condition evaluation, dispatch to action/script execution, continue-on-error, and post-step draining. |
| `workflow.rs` | (mod decl) | Declares the `workflow` sub-module; see its own README. |
| `workspace_gc.rs` | `gc_workspaces` | Prunes `workspace_root/<job_id>` directories older than a configured age, always sparing the currently-running job. |

## Sub-modules

- `actions/` — action resolution, download, and manifest parsing for `uses:` steps.
- `artifacts/` — artifact upload/download service (Azure append-blob backend).
- `handlers/` — the concrete `runs.using` handlers plus dispatch resolution.
- `oidc/` — OIDC token server and claims building.
- `shadow/` — shadow-mode (approach C) workspace fingerprinting; records only.
- `workflow/` — workflow YAML parsing, matrix expansion, orchestration, and reusable-workflow resolution.

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
