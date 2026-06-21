//! Runs a single Node.js action stage (`pre` / `main` / `post`).
//!
//! A node action can define up to three entrypoints. Each stage runs in the
//! *same* step scope (same `step.id`), so `save-state` written by `main`
//! surfaces as `STATE_*` to that step's `pre`/`post` — `build_node_env`
//! injects the step's accumulated state on every stage. Stdout workflow
//! commands are dispatched onto the live context after the process exits.

use std::collections::HashMap;
use std::path::Path;

use shared::{ActionStep, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;

use super::action_support::{build_node_env, emit_log};
use super::actions::manifest::ActionDefinition;
use super::command_dispatch::stream_dispatch_stdout;
use super::context::ExecutionContext;
use super::handlers::node::determine_script;
use super::handlers::node_exec::{NodeExecParams, execute_node_action};
use super::step_timeout::StepBounds;
use crate::node::runtime::ensure_node_runtime;

/// Inputs for running one Node.js action stage.
pub(super) struct NodeStage<'a> {
  pub step: &'a ActionStep,
  pub ctx: &'a mut ExecutionContext,
  pub events: &'a mpsc::Sender<RunnerEvent>,
  pub workspace: &'a Path,
  pub config: &'a RunnerConfig,
  pub client: &'a reqwest::Client,
  pub action_dir: &'a Path,
  pub manifest: &'a ActionDefinition,
  pub major: u8,
  /// Timeout / cancellation bounds applied to the spawned node child.
  pub bounds: &'a StepBounds,
  /// `"pre"`, `"main"`, or `"post"`.
  pub stage: &'a str,
}

/// Resolve the stage's entrypoint to an existing on-disk script path.
///
/// # Errors
///
/// Returns `RunnerError::ActionManifest` when the stage has no entrypoint or
/// the resolved script file does not exist.
fn resolve_stage_script(s: &NodeStage<'_>) -> Result<std::path::PathBuf, RunnerError> {
  let Some(rel) = determine_script(s.manifest, s.stage) else {
    return Err(RunnerError::ActionManifest(format!(
      "node action has no '{}' entrypoint",
      s.stage
    )));
  };
  let script = s.action_dir.join(&rel);
  if !script.exists() {
    return Err(RunnerError::ActionManifest(format!(
      "{} script not found: {}",
      s.stage,
      script.display()
    )));
  }
  Ok(script)
}

/// Run one node entrypoint, returning its conclusion and the `set-output`
/// values it emitted (so a `main` stage's stdout `::set-output::` can reach
/// `StepCompleted.outputs`, consistent with `ctx`). Env is rebuilt per stage
/// so the latest `STATE_*` is visible.
///
/// # Errors
///
/// Returns `RunnerError` if the node runtime is unavailable, the script is
/// missing, or the process cannot be spawned/awaited.
pub(super) async fn run_node_stage(
  s: NodeStage<'_>,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let script = resolve_stage_script(&s)?;
  let node_binary = ensure_node_runtime(s.client, &s.config.data_dir, s.major).await?;
  let env = build_node_env(
    s.step,
    s.ctx,
    s.manifest,
    s.action_dir,
    s.workspace,
    s.config,
  );

  // Own the cgroup path so `node_params` doesn't borrow `s.ctx` — the
  // concurrent dispatcher needs `&mut s.ctx` while the child runs.
  let cgroup = s.ctx.cgroup_path().map(Path::to_path_buf);
  let node_params = NodeExecParams {
    node_binary: &node_binary,
    script_path: &script,
    env: &env,
    working_dir: s.workspace,
    step_id: &s.step.id,
    cgroup_path: cgroup.as_deref(),
    timeout: s.bounds.timeout,
    cancel: &s.bounds.cancel,
  };

  // Stream the action's stdout through the dispatcher as it runs (realtime
  // `Log` events), mirroring the run-step path. `execute_node_action` owns the
  // only producer copy, so the dispatcher's `recv` closes when the child EOFs.
  let (stdout_tx, mut stdout_rx) = tokio_mpsc::channel::<String>(256);
  let exec = execute_node_action(&node_params, s.events, stdout_tx);
  let dispatch = stream_dispatch_stdout(&s.step.id, &mut stdout_rx, s.ctx, s.events);
  let (output, outputs) = tokio::join!(exec, dispatch);
  Ok((output?.conclusion, outputs))
}

/// Emit the per-stage `##[endgroup]` separator before a node entrypoint runs.
pub(super) async fn emit_stage_endgroup(events: &mpsc::Sender<RunnerEvent>, step_id: &str) {
  emit_log(events, step_id, "##[endgroup]").await;
}
