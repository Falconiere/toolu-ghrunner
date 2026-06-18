use std::collections::HashMap;
use std::path::Path;

use shared::{ActionStep, Conclusion, LogStream, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_exec::execute_action;
use super::context::ExecutionContext;
use super::file_commands::FileCommandManager;
use super::handlers::script::{ScriptHandler, ScriptParams};
use super::step_env::{apply_file_commands, resolve_step_env};
use super::step_naming::derive_step_name;

// Re-export post-step types so callers don't need to change imports.
pub use super::step_naming::{PostStep, PostStepQueue};

/// Constant-per-job context passed to each step.
struct JobCtx<'a> {
  handler: ScriptHandler,
  workspace: &'a Path,
  config: &'a RunnerConfig,
}

/// Run a sequence of steps with condition evaluation and error handling.
///
/// # Errors
///
/// Returns `RunnerError` if an unrecoverable error occurs.
pub async fn run_steps(
  steps: &[ActionStep],
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  cancel: CancellationToken,
  workspace: &Path,
  config: &RunnerConfig,
) -> Result<Conclusion, RunnerError> {
  if cancel.is_cancelled() {
    return Ok(Conclusion::Cancelled);
  }

  let job = JobCtx {
    handler: ScriptHandler::new(),
    workspace,
    config,
  };

  let mut job_conclusion = Conclusion::Success;

  for (index, step) in steps.iter().enumerate() {
    if cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }

    // Step 1 is "Set up job" (reported by setup_step.rs). Workflow steps start at 2.
    let step_number = u32::try_from(index + 2).unwrap_or(0);
    let step_conclusion = run_single_step(step, step_number, ctx, events, &job).await?;

    if step_conclusion == Conclusion::Failure {
      job_conclusion = Conclusion::Failure;
    }
  }

  Ok(job_conclusion)
}

async fn run_single_step(
  step: &ActionStep,
  step_number: u32,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
) -> Result<Conclusion, RunnerError> {
  if !evaluate_condition(step, ctx)? {
    let _ = events
      .send(RunnerEvent::StepSkipped {
        step_id: step.id.clone(),
        reason: "condition evaluated to false".to_owned(),
      })
      .await;
    return Ok(Conclusion::Success);
  }
  let _ = events
    .send(RunnerEvent::StepStarted {
      step_id: step.id.clone(),
      step_name: derive_step_name(step),
      step_number,
    })
    .await;

  let (result, outputs) = execute_step(step, ctx, events, job).await?;
  let effective = apply_continue_on_error(step, result, ctx);
  ctx.set_step_conclusion(&step.id, effective);

  let _ = events
    .send(RunnerEvent::StepCompleted {
      step_id: step.id.clone(),
      conclusion: effective,
      outputs,
    })
    .await;
  Ok(effective)
}

fn evaluate_condition(step: &ActionStep, ctx: &ExecutionContext) -> Result<bool, RunnerError> {
  let condition = step.condition.as_deref().unwrap_or("success()");

  if condition.is_empty() {
    return Ok(true);
  }

  let result = ctx.evaluate_expression(condition)?;
  Ok(result.is_truthy())
}

async fn execute_step(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let is_run = step.is_run_step();
  tracing::info!(step_id = step.id.as_str(), is_run, "executing step");

  if !is_run {
    let conclusion = execute_action(step, ctx, events, job.workspace, job.config).await?;
    return Ok((conclusion, HashMap::new()));
  }

  let script = step.script_body().unwrap_or_default();
  let interpolated = ctx.interpolate_string(&script)?;
  let shell = step.shell_name();
  let step_env = resolve_step_env(step, ctx)?;
  let mut env = ctx.build_step_env(&step_env);
  let tmp_dir = job.config.data_dir.join("tmp");
  std::fs::create_dir_all(&tmp_dir)?;
  let (file_cmds, file_cmd_env) = FileCommandManager::create(&tmp_dir)?;
  env.extend(file_cmd_env);
  for (k, v) in std::env::vars() {
    env.entry(k).or_insert(v);
  }
  emit_log(
    events,
    &step.id,
    &format!("##[group]Run {}", interpolated.trim()),
  )
  .await;
  emit_log(events, &step.id, "##[endgroup]").await;
  let params = ScriptParams {
    script: &interpolated,
    shell: shell.as_deref(),
    env: &env,
    working_dir: job.workspace,
    step_id: &step.id,
    cgroup_path: ctx.cgroup_path(),
  };

  let result = job.handler.execute(&params, events).await?;
  let outputs = apply_file_commands(&file_cmds, ctx);

  // Emit exit status so steps with no output still show a result
  let status_msg = if result == Conclusion::Success {
    "Process completed with exit code 0."
  } else {
    "Process completed with exit code 1."
  };
  emit_log(events, &step.id, status_msg).await;
  Ok((result, outputs))
}

fn apply_continue_on_error(
  step: &ActionStep,
  result: Conclusion,
  ctx: &mut ExecutionContext,
) -> Conclusion {
  let continue_on_error = step.continue_on_error.unwrap_or(false);
  if result == Conclusion::Failure && continue_on_error {
    // Step outcome is Failure, but job continues as Success
    return Conclusion::Success;
  }
  if result == Conclusion::Failure {
    ctx.record_step_failure();
  }
  result
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
