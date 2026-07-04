//! Composite action executor.
//!
//! Runs each step in a composite action's `steps:` array as a shell subprocess,
//! managing `GITHUB_OUTPUT`, `GITHUB_ENV`, and `GITHUB_PATH` file commands
//! between steps.

use std::collections::HashMap;
use std::path::Path;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

pub use super::composite_env::{CompositeParams, CompositeResult};
use super::composite_env::{
  build_step_env, create_file_command_files, process_file_commands, should_skip_step,
};
use super::composite_expr::interpolate_composite_expr;
use super::composite_shell::{ShellScriptParams, run_shell_script};
use super::composite_uses::{NestedUsesParams, run_nested_uses_step};
use super::context::ExecutionContext;
use super::depth_tracker::DepthTracker;

/// Execute a composite action's steps sequentially.
///
/// # Errors
///
/// Returns `RunnerError::StepExecution` on subprocess spawn failures.
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
  };

  for (idx, step) in params.manifest.runs.steps.iter().enumerate() {
    let skip = should_skip_step(step);

    let conclusion = if step.uses.is_some() {
      run_uses_step(&mut run, step, idx, depth, skip).await?
    } else if let Some(script) = &step.run {
      if skip {
        continue;
      }
      run_run_step(&mut run, step, idx, script).await?
    } else {
      continue;
    };

    if conclusion == Conclusion::Failure && !step.continue_on_error {
      return Ok(state.into_result(Conclusion::Failure));
    }
    // A cancelled nested step means the job cancel token fired: stop the
    // composite and surface `Cancelled` so the parent stops too.
    if conclusion == Conclusion::Cancelled {
      return Ok(state.into_result(Conclusion::Cancelled));
    }
  }

  Ok(state.into_result(Conclusion::Success))
}

/// Mutable working set for one composite action's step loop: the read-only
/// bundle, the live context, the temp dir, and the cross-step state.
struct CompositeRun<'a> {
  params: &'a CompositeParams<'a>,
  ctx: &'a mut ExecutionContext,
  temp_dir: &'a Path,
  state: &'a mut CompositeState,
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
  fn into_result(self, conclusion: Conclusion) -> CompositeResult {
    CompositeResult {
      conclusion,
      env_additions: self.extra_env,
      path_additions: self.path_additions,
    }
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
  );
  let file_paths = create_file_command_files(run.temp_dir, &step_id)?;
  let full_env = merge_file_command_env(&env, &file_paths);

  let interpolated = interpolate_composite_expr(
    script,
    params.step_inputs,
    &run.state.step_outputs,
    &env,
    run.temp_dir,
  );
  let conclusion = run_step_shell(params, run.ctx, step, &interpolated, &full_env).await?;

  emit_log(params.events, params.parent_step_id, "##[endgroup]").await;
  process_file_commands(
    &file_paths,
    &step_id,
    &mut run.state.step_outputs,
    &mut run.state.extra_env,
    &mut run.state.path_additions,
  );

  Ok(conclusion)
}

/// Append `##[group]Run {name}` for a composite step.
async fn emit_run_group(events: &mpsc::Sender<RunnerEvent>, parent_step_id: &str, step_name: &str) {
  emit_log(events, parent_step_id, &format!("##[group]Run {step_name}")).await;
}

/// Spawn the shell subprocess for a composite `run:` step.
async fn run_step_shell(
  params: &CompositeParams<'_>,
  ctx: &ExecutionContext,
  step: &super::actions::manifest::CompositeStep,
  script: &str,
  env: &HashMap<String, String>,
) -> Result<Conclusion, RunnerError> {
  let shell = step.shell.as_deref().unwrap_or("bash");
  let shell_params = ShellScriptParams {
    shell,
    script,
    env,
    working_dir: params.workspace,
    log_step_id: params.parent_step_id,
    cgroup_path: ctx.cgroup_path(),
  };
  run_shell_script(&shell_params, params.events).await
}

/// Run a nested composite `uses:` step, recursing through the action engine.
async fn run_uses_step(
  run: &mut CompositeRun<'_>,
  step: &super::actions::manifest::CompositeStep,
  idx: usize,
  depth: &mut DepthTracker,
  skip: bool,
) -> Result<Conclusion, RunnerError> {
  let params = run.params;
  let nested = NestedUsesParams {
    step,
    idx,
    inputs: params.step_inputs,
    step_outputs: &run.state.step_outputs,
    ctx: run.ctx,
    events: params.events,
    workspace: params.workspace,
    config: params.config,
    depth,
    temp_dir: run.temp_dir,
    cancel: params.cancel,
  };
  run_nested_uses_step(nested, skip).await
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

async fn emit_log(events: &mpsc::Sender<RunnerEvent>, step_id: &str, line: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: line.to_owned(),
      stream: LogStream::Stdout,
    })
    .await;
}
