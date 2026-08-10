//! Job execution engine.
//!
//! Populated in steps 4c + 4d.

/// Downloads and executes `uses:` actions.
pub mod action_exec;
mod action_support;
/// Resolves and downloads `uses:` actions (`resolver`, `downloader`, `manifest`).
pub mod actions;
/// Artifact upload/download via Azure append-blob (`backend` + `service`).
pub mod artifacts;
/// Spawns step processes joined into the per-job cgroup (v2).
pub mod cgroup_join;
/// Consumes workflow commands from a step's stdout into the live execution context.
pub mod command_dispatch;
/// Parses GitHub Actions workflow commands (`::set-output::`, `::add-mask::`, ...) from step stdout.
pub mod command_parser;
mod composite_env;
/// Runs a composite action's `steps:` array as shell subprocesses.
pub mod composite_exec;
/// Interpolates `${{ }}` expressions inside composite-action step strings.
pub mod composite_expr;
/// Output-isolation scoping so nested composite actions don't leak each other's outputs.
pub mod composite_scope;
/// Runs a composite step's shell script as a subprocess, streaming its output.
pub mod composite_shell;
mod composite_uses;
/// Step environment, secrets and masking built for the running job.
pub mod context;
/// Builds the full `${{ }}` evaluation context for a step.
pub(crate) mod context_build;
/// Tracks composite-action nesting depth against `MAX_COMPOSITE_DEPTH`.
pub mod depth_tracker;
/// Parses the `GITHUB_ENV` / `GITHUB_PATH` / `GITHUB_OUTPUT` / `GITHUB_STATE` file commands.
pub mod file_commands;
/// Handler dispatch by `runs.using` (plugin → script → node → docker → composite).
pub mod handlers;
/// Job-level `ACTIONS_RUNNER_HOOK_JOB_*` hooks (self-hosted job lifecycle scripts).
pub mod job_hooks;
/// The job execution entry point (`run_job`) and per-job directory setup.
pub mod job_runner;
/// Job-level `outputs:` and the merged `defaults.run` fallback.
pub mod job_spec;
/// Deferred post-completion teardown (cache maintenance + workspace sweep).
pub mod job_teardown;
mod node_stage;
/// OIDC token service for runner execution.
pub mod oidc;
mod post_drain;
/// Bearer-token validation shared across the local OIDC/artifact/cache services.
pub mod service_auth;
/// Forwarder/offline/accelerated service-URL injection for artifacts, cache and OIDC.
pub mod service_endpoints;
/// Shared lifecycle management for local HTTP micro-services (artifact, cache, OIDC).
pub mod service_lifecycle;
/// Shadow-mode step observation (approach C): records only, never serves.
pub mod shadow;
mod step_env;
mod step_naming;
mod step_state;
/// Bounded child-process wait shared by the script and node handlers.
pub mod step_timeout;
/// Runs the per-job step loop.
pub mod steps_runner;
/// Workflow YAML parsing, matrix build, orchestration and reusable-workflow resolution.
pub mod workflow;
/// Age-based garbage collection of per-job workspace directories.
pub mod workspace_gc;
