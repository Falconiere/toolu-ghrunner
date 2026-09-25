//! Composite action executor.
//!
//! Runs each step in a composite action's `steps:` array as a shell subprocess,
//! managing `GITHUB_OUTPUT`, `GITHUB_ENV`, and `GITHUB_PATH` file commands
//! between steps.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use expressions::evaluator::JobStatus;
use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use super::composite_env::{CompositeParams, CompositeResult};
use super::composite_env::{build_step_env, create_file_command_files, process_file_commands};
use super::composite_expr::{
  CompositeField, composite_eval_context, evaluate_composite_condition, interpolate_composite_expr,
};
use super::composite_shell::{ShellScriptParams, run_shell_script};
use super::composite_uses::{NestedUsesParams, run_nested_uses_step};
use super::context::ExecutionContext;
use super::depth_tracker::DepthTracker;

/// Execute a composite action's steps sequentially.
///
/// A hard error mid-inner-step is reported via [`report_composite_step_error`]
/// and folded into the same `continue-on-error` handling as a step failure,
/// matching GitHub's semantics for a failed nested step (see that fn's docs).
///
/// # Errors
///
/// Returns `RunnerError` if the composite's temp dir cannot be created.
pub async fn execute_composite_action(
  params: &CompositeParams<'_>,
  ctx: &mut ExecutionContext,
  depth: &mut DepthTracker,
) -> Result<CompositeResult, RunnerError> {
  let temp_dir = params.config.data_dir.join("tmp");
  std::fs::create_dir_all(&temp_dir)?;

  let mut state = CompositeState::default();
  let mut run = CompositeRun {
    params,
    ctx,
    temp_dir: &temp_dir,
    state: &mut state,
    cleanup_token: None,
    cleanup_deadline: None,
  };

  let aggregate = run_composite_steps(&mut run, depth).await;
  run.state.result(aggregate, params, run.ctx)
}

async fn run_composite_steps(run: &mut CompositeRun<'_>, depth: &mut DepthTracker) -> Conclusion {
  let mut aggregate = Conclusion::Success;
  for (idx, step) in run.params.manifest.runs.steps.iter().enumerate() {
    let eval_ctx = composite_eval_context(run.ctx, run.params.step_inputs, None);
    match evaluate_composite_condition(step.condition.as_deref(), &eval_ctx) {
      Ok(false) => {
        record_inner_result(run.ctx, step, Conclusion::Skipped, Conclusion::Skipped);
        continue;
      },
      Err(err) => {
        report_composite_step_error(run.params.events, run.params.parent_step_id, &err).await;
        record_inner_result(run.ctx, step, Conclusion::Failure, Conclusion::Failure);
        if aggregate != Conclusion::Cancelled {
          aggregate = Conclusion::Failure;
        }
        break;
      },
      Ok(true) => {},
    }
    let Some(outcome) = run_one_step(run, step, idx, depth).await else {
      continue;
    };
    let conclusion = if outcome == Conclusion::Failure && step.continue_on_error {
      Conclusion::Success
    } else {
      outcome
    };
    record_inner_result(run.ctx, step, outcome, conclusion);
    if conclusion == Conclusion::Failure {
      if aggregate != Conclusion::Cancelled {
        aggregate = Conclusion::Failure;
        run.ctx.set_scope_status(JobStatus::Failure);
      }
    } else if conclusion == Conclusion::Cancelled {
      aggregate = Conclusion::Cancelled;
      run.ctx.set_scope_status(JobStatus::Cancelled);
      run.cleanup_token = Some(CancellationToken::new());
      run.cleanup_deadline = Some(
        run
          .params
          .deadline
          .unwrap_or_else(|| Instant::now() + Duration::from_secs(300)),
      );
    }
  }
  aggregate
}

fn record_inner_result(
  ctx: &mut ExecutionContext,
  step: &super::actions::manifest::CompositeStep,
  outcome: Conclusion,
  conclusion: Conclusion,
) {
  if let Some(name) = step.id.as_deref().filter(|name| !name.is_empty()) {
    ctx.set_step_outcome(name, outcome);
    ctx.set_step_conclusion(name, conclusion);
  }
}

/// Dispatch one eligible composite step; `None` when it has no run or uses body.
async fn run_one_step(
  run: &mut CompositeRun<'_>,
  step: &super::actions::manifest::CompositeStep,
  idx: usize,
  depth: &mut DepthTracker,
) -> Option<Conclusion> {
  let params = run.params;

  if step.uses.is_some() {
    match run_uses_step(run, step, idx, depth).await {
      Ok(c) => Some(c),
      Err(err) => {
        Some(report_composite_step_error(params.events, params.parent_step_id, &err).await)
      },
    }
  } else if let Some(script) = &step.run {
    match run_run_step(run, step, idx, script).await {
      Ok(c) => Some(c),
      Err(err) => {
        Some(report_composite_step_error(params.events, params.parent_step_id, &err).await)
      },
    }
  } else {
    None
  }
}

/// Mutable working set for one composite action's step loop: the read-only
/// bundle, the live context, the file-command temp dir, and the cross-step
/// state.
///
/// `temp_dir` (`data_dir/tmp`) only backs the `$GITHUB_OUTPUT`/`ENV`/`PATH`
/// file commands — distinct from `$RUNNER_TEMP` / `${{ runner.temp }}`,
/// which both `composite_env.rs` (env) and `composite_expr.rs` via `ctx`
/// (interpolation) source from `set_runner_context`'s `data_dir/_temp`
/// instead (B-004, B-005).
struct CompositeRun<'a> {
  params: &'a CompositeParams<'a>,
  ctx: &'a mut ExecutionContext,
  temp_dir: &'a Path,
  state: &'a mut CompositeState,
  cleanup_token: Option<CancellationToken>,
  cleanup_deadline: Option<Instant>,
}

impl CompositeRun<'_> {
  fn active_cancel(&self) -> CancellationToken {
    self
      .cleanup_token
      .clone()
      .unwrap_or_else(|| self.params.cancel.clone())
  }

  fn active_deadline(&self) -> Option<Instant> {
    self.cleanup_deadline.or(self.params.deadline)
  }
}

/// Mutable state threaded across composite steps: per-step outputs and the
/// env/path additions to propagate to the parent.
#[derive(Default)]
struct CompositeState {
  step_outputs: HashMap<String, HashMap<String, String>>,
  extra_env: HashMap<String, String>,
  path_additions: Vec<String>,
}

impl CompositeState {
  fn result(
    &self,
    conclusion: Conclusion,
    params: &CompositeParams<'_>,
    ctx: &ExecutionContext,
  ) -> Result<CompositeResult, RunnerError> {
    let eval_ctx = composite_eval_context(ctx, params.step_inputs, None);
    let mut outputs = HashMap::new();
    for (name, output) in &params.manifest.outputs {
      if let Some(value) = &output.value {
        outputs.insert(
          name.clone(),
          interpolate_composite_expr(value, &eval_ctx, CompositeField::Output)?,
        );
      }
    }
    Ok(CompositeResult {
      conclusion,
      outputs,
      env_additions: self.extra_env.clone(),
      path_additions: self.path_additions.clone(),
    })
  }
}

/// Run a composite `run:` (shell) step, capturing its file-command outputs.
async fn run_run_step(
  run: &mut CompositeRun<'_>,
  step: &super::actions::manifest::CompositeStep,
  idx: usize,
  script: &str,
) -> Result<Conclusion, RunnerError> {
  let params = run.params;
  let step_id = step
    .id
    .clone()
    .unwrap_or_else(|| format!("__composite_{idx}"));
  let step_name = step
    .name
    .as_deref()
    .unwrap_or_else(|| script.lines().next().unwrap_or("(composite step)"));
  emit_run_group(params.events, params.parent_step_id, step_name).await;

  let env = build_step_env(
    params,
    run.ctx,
    step,
    &run.state.extra_env,
    &run.state.path_additions,
  )?;
  let file_paths = create_file_command_files(run.temp_dir, &step_id)?;
  let full_env = merge_file_command_env(&env, &file_paths);

  let eval_ctx = composite_eval_context(run.ctx, params.step_inputs, Some(&env));
  let interpolated = interpolate_composite_expr(script, &eval_ctx, CompositeField::Step)?;
  let shell = interpolate_composite_expr(
    step.shell.as_deref().unwrap_or("bash"),
    &eval_ctx,
    CompositeField::Step,
  )?;
  let conclusion = run_step_shell(run, &shell, &interpolated, &full_env).await?;

  emit_log(params.events, params.parent_step_id, "##[endgroup]").await;
  process_file_commands(
    &file_paths,
    &step_id,
    &mut run.state.step_outputs,
    &mut run.state.extra_env,
    &mut run.state.path_additions,
  );
  apply_run_file_commands(run, &step_id);

  Ok(conclusion)
}

fn apply_run_file_commands(run: &mut CompositeRun<'_>, step_id: &str) {
  if let Some(outputs) = run.state.step_outputs.get(step_id) {
    for (key, value) in outputs {
      run.ctx.set_step_output(step_id, key, value);
    }
  }
  for (key, value) in &run.state.extra_env {
    run.ctx.set_env(key, value);
  }
}

/// Append `##[group]Run {name}` for a composite step.
async fn emit_run_group(events: &mpsc::Sender<RunnerEvent>, parent_step_id: &str, step_name: &str) {
  emit_log(events, parent_step_id, &format!("##[group]Run {step_name}")).await;
}

/// Spawn the shell subprocess for a composite `run:` step.
async fn run_step_shell(
  run: &CompositeRun<'_>,
  shell: &str,
  script: &str,
  env: &HashMap<String, String>,
) -> Result<Conclusion, RunnerError> {
  let params = run.params;
  let cancel = run.active_cancel();
  let shell_params = ShellScriptParams {
    shell,
    script,
    env,
    working_dir: params.workspace,
    log_step_id: params.parent_step_id,
    cgroup_path: run.ctx.cgroup_path(),
    timeout: run
      .active_deadline()
      .map(|deadline| deadline.saturating_duration_since(Instant::now())),
    cancel: &cancel,
  };
  run_shell_script(&shell_params, params.events).await
}

/// Run a nested composite `uses:` step, recursing through the action engine.
async fn run_uses_step(
  run: &mut CompositeRun<'_>,
  step: &super::actions::manifest::CompositeStep,
  idx: usize,
  depth: &mut DepthTracker,
) -> Result<Conclusion, RunnerError> {
  let params = run.params;
  let cancel = run.active_cancel();
  let deadline = run.active_deadline();
  let nested = NestedUsesParams {
    step,
    idx,
    inputs: params.step_inputs,
    ctx: run.ctx,
    events: params.events,
    workspace: params.workspace,
    config: params.config,
    depth,
    cancel: &cancel,
    deadline,
    http: params.http,
    fetcher: params.fetcher,
    parent_step_id: params.parent_step_id,
  };
  let (conclusion, outputs) = run_nested_uses_step(nested).await?;
  if let Some(name) = step.id.as_deref() {
    run.state.step_outputs.insert(name.to_owned(), outputs);
  }
  Ok(conclusion)
}

fn merge_file_command_env(
  base: &HashMap<String, String>,
  files: &super::composite_env::FileCommandPaths,
) -> HashMap<String, String> {
  let mut env = base.clone();
  env.insert(
    "GITHUB_OUTPUT".to_owned(),
    files.output.to_string_lossy().into_owned(),
  );
  env.insert(
    "GITHUB_ENV".to_owned(),
    files.env.to_string_lossy().into_owned(),
  );
  env.insert(
    "GITHUB_PATH".to_owned(),
    files.path.to_string_lossy().into_owned(),
  );
  env
}

/// Report a hard error that escaped an inner composite step (action
/// resolution, manifest read, node runtime): these short-circuit before a
/// `Conclusion` exists, unlike a subprocess/action exit that always yields
/// one. The composite's own parent step is still `in_progress` on GitHub —
/// no per-inner-step `StepCompleted` exists to close out (composite inner
/// steps have no GH step of their own) — so this emits only the `##[error]`
/// line, scoped to the parent step id, and returns `Failure`. The caller
/// records the failed outcome, applies `continue-on-error`, and evaluates
/// later eligible cleanup conditions.
async fn report_composite_step_error(
  events: &mpsc::Sender<RunnerEvent>,
  parent_step_id: &str,
  err: &RunnerError,
) -> Conclusion {
  // Also to the diag log/journal: the ##[error] line is the only durable
  // sink otherwise, and the error's detail outlives the live stream here.
  tracing::warn!(parent_step_id, error = %err, "composite inner step hard error");
  emit_log(events, parent_step_id, &format!("##[error]{err}")).await;
  Conclusion::Failure
}

async fn emit_log(events: &mpsc::Sender<RunnerEvent>, step_id: &str, line: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: line.to_owned(),
      stream: LogStream::Stdout,
    })
    .await;
}
