use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::{ActionStep, Conclusion, LogStream, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_exec::{ActionOutcome, ActionRun, execute_action};
use super::command_dispatch::stream_dispatch_stdout;
use super::context::ExecutionContext;
use super::depth_tracker::DepthTracker;
use super::file_commands::FileCommandManager;
use super::handlers::script::{ScriptHandler, ScriptParams};
use super::job_spec::JobSpec;
use super::post_drain::drain_post_steps;
use super::step_env::{apply_file_commands, resolve_step_env};
use super::step_naming::derive_step_name;
use super::step_timeout::StepBounds;

// Re-export post-step types so callers don't need to change imports.
pub use super::step_naming::{PostStep, PostStepQueue};

/// Constant-per-job context passed to each step.
pub(super) struct JobCtx<'a> {
  handler: ScriptHandler,
  pub(super) workspace: &'a Path,
  pub(super) config: &'a RunnerConfig,
  /// The job's in-flight cancellation token (one per job; per-step bounds
  /// clone it). A fired token kills the running step's child.
  pub(super) cancel: &'a CancellationToken,
  /// Job-level `defaults.run` fallback for run-steps that omit shell /
  /// working-directory (merged workflow + job defaults).
  pub(super) job: &'a JobSpec,
}

/// Job-constant inputs for a run: the workspace root, runner config, and the
/// job's `outputs:`/`defaults.run` spec. Grouped so `run_steps` stays within
/// the argument-count budget.
pub struct JobRun<'a> {
  pub workspace: &'a Path,
  pub config: &'a RunnerConfig,
  pub spec: &'a JobSpec,
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
  run: &JobRun<'_>,
) -> Result<Conclusion, RunnerError> {
  if cancel.is_cancelled() {
    return Ok(Conclusion::Cancelled);
  }

  let job = JobCtx {
    handler: ScriptHandler::new(),
    workspace: run.workspace,
    config: run.config,
    cancel: &cancel,
    job: run.spec,
  };

  let mut job_state = JobState {
    posts: PostStepQueue::new(),
    // Bounds composite `uses:` recursion across the whole job (reset per step
    // since top-level steps are not nested in one another).
    depth: DepthTracker::new(),
  };

  // Held as a `Result` (not unwrapped) so the drain below runs even when the
  // main loop returned a hard `Err` — a spawn/I-O error must not abandon
  // registered posts.
  let main_result = run_main_steps(steps, ctx, events, &cancel, &job, &mut job_state).await;

  // Drain post-steps LIFO AFTER all main steps — including when a prior step
  // failed or errored hard. Each post-if is evaluated against the live
  // job/steps status, so `always()` posts run on failure while
  // `success()`/`failure()` honor it. Best-effort: a failing post-step must
  // not overwrite the job's conclusion (a `Failure` from a main step must
  // survive a post-step error).
  if let Err(e) = drain_post_steps(&mut job_state.posts, ctx, events, &job).await {
    tracing::error!(error = ?e, "post-step drain failed; preserving job conclusion");
  }

  main_result
}

/// Run the main step loop, draining posts on cancel. Returns the aggregate
/// main-step conclusion (`Failure` if any step failed, `Cancelled` on cancel).
async fn run_main_steps(
  steps: &[ActionStep],
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  cancel: &CancellationToken,
  job: &JobCtx<'_>,
  job_state: &mut JobState,
) -> Result<Conclusion, RunnerError> {
  let mut job_conclusion = Conclusion::Success;
  for (index, step) in steps.iter().enumerate() {
    if cancel.is_cancelled() {
      // Still drain registered post-steps so action cleanup runs on cancel.
      // Best-effort: a failing post-step must not mask the `Cancelled` result.
      if let Err(e) = drain_post_steps(&mut job_state.posts, ctx, events, job).await {
        tracing::error!(error = ?e, "post-step drain on cancel failed; still cancelling");
      }
      return Ok(Conclusion::Cancelled);
    }

    // Step 1 is "Set up job" (reported by setup_step.rs). Workflow steps start at 2.
    let step_number = u32::try_from(index + 2).unwrap_or(0);
    let step_conclusion = run_single_step(step, step_number, ctx, events, job, job_state).await?;

    if step_conclusion == Conclusion::Failure {
      job_conclusion = Conclusion::Failure;
    }
    // A `Cancelled` step conclusion only arises when the job cancel token
    // fired mid-step (its child was killed); surface it without waiting for
    // the next-iteration token check. Post-steps are drained by the caller.
    if step_conclusion == Conclusion::Cancelled {
      return Ok(Conclusion::Cancelled);
    }
  }
  Ok(job_conclusion)
}

/// Job-level mutable accumulators threaded through the step loop: the LIFO
/// post-step queue and the composite recursion depth tracker.
struct JobState {
  posts: PostStepQueue,
  depth: DepthTracker,
}

async fn run_single_step(
  step: &ActionStep,
  step_number: u32,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
  job_state: &mut JobState,
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

  let bounds = StepBounds::new(step.timeout_in_minutes, job.cancel.clone());
  let (outcome, outputs) = execute_step(step, ctx, events, job, job_state, &bounds).await?;
  // `outcome` is the real result; `conclusion` is continue-on-error-adjusted.
  // Both are recorded so `steps.<id>.outcome` and `.conclusion` can differ.
  ctx.set_step_outcome(&step.id, outcome);
  let conclusion = apply_continue_on_error(step, outcome, ctx);
  ctx.set_step_conclusion(&step.id, conclusion);

  let _ = events
    .send(RunnerEvent::StepCompleted {
      step_id: step.id.clone(),
      conclusion,
      outputs,
    })
    .await;
  Ok(conclusion)
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
  job_state: &mut JobState,
  bounds: &StepBounds,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let is_run = step.is_run_step();
  tracing::info!(step_id = step.id.as_str(), is_run, "executing step");

  if !is_run {
    let run = ActionRun {
      events,
      workspace: job.workspace,
      config: job.config,
      bounds,
    };
    let ActionOutcome {
      conclusion,
      post,
      outputs,
    } = execute_action(step, ctx, &run, &mut job_state.depth).await?;
    // Register the action's `post` entrypoint to drain LIFO at job end.
    if let Some(post_step) = post {
      job_state.posts.register(post_step);
    }
    return Ok((conclusion, outputs));
  }

  run_script_step(step, ctx, events, job, bounds).await
}

/// Run a `run:` step: build env + file commands, execute the shell, dispatch
/// stdout workflow commands, and merge step outputs.
async fn run_script_step(
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  job: &JobCtx<'_>,
  bounds: &StepBounds,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let script = step.script_body().unwrap_or_default();
  let interpolated = ctx.interpolate_string(&script)?;
  // Shell precedence: step `shell:` > job/workflow `defaults.run.shell`.
  let shell = step.shell_name().or_else(|| job.job.defaults.shell.clone());
  let (env, file_cmds) = build_step_env_and_file_commands(step, ctx, job)?;
  let working_dir = resolve_working_dir(step, ctx, job.workspace, &job.job.defaults)?;

  emit_log(
    events,
    &step.id,
    &format!("##[group]Run {}", interpolated.trim()),
  )
  .await;
  emit_log(events, &step.id, "##[endgroup]").await;
  // Own the cgroup path so `params` doesn't borrow `ctx` — the concurrent
  // dispatcher needs `&mut ctx` while the child runs.
  let cgroup = ctx.cgroup_path().map(Path::to_path_buf);
  let params = ScriptParams {
    script: &interpolated,
    shell: shell.as_deref(),
    env: &env,
    working_dir: &working_dir,
    step_id: &step.id,
    cgroup_path: cgroup.as_deref(),
    timeout: bounds.timeout,
    cancel: &bounds.cancel,
  };

  let (result, stdout_outputs) =
    run_and_dispatch_script(&job.handler, &params, &step.id, ctx, events).await?;
  let outputs = merge_step_outputs(step, stdout_outputs, &file_cmds, ctx).await;

  let status_msg = if result == Conclusion::Success {
    "Process completed with exit code 0."
  } else {
    "Process completed with exit code 1."
  };
  emit_log(events, &step.id, status_msg).await;
  Ok((result, outputs))
}

/// Run the shell child and stream-dispatch its stdout concurrently.
///
/// The handler forwards each stdout line onto `stdout_tx` as it is read, while
/// `stream_dispatch_stdout` (owning `&mut ctx`) dispatches commands and emits
/// `Log` events in realtime. `execute` borrows only params/events/sender, so
/// it does not conflict with the concurrent `&mut ctx` consumer. Returns the
/// exit conclusion and the `set-output` map.
async fn run_and_dispatch_script(
  handler: &ScriptHandler,
  params: &ScriptParams<'_>,
  step_id: &str,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(256);
  // `execute` takes the sender by value and owns the only producer copy; when
  // its future completes (after EOF) the sender drops, so the dispatcher's
  // `recv` sees channel close and returns its `set-output` map.
  let exec = handler.execute(params, events, stdout_tx);
  let dispatch = stream_dispatch_stdout(step_id, &mut stdout_rx, ctx, events);
  let (exec_result, stdout_outputs) = tokio::join!(exec, dispatch);
  Ok((exec_result?.conclusion, stdout_outputs))
}

/// Merge a script step's stdout `set-output` outputs (already applied to `ctx`
/// by the streaming dispatcher) with its `$GITHUB_OUTPUT` file-command outputs
/// into one step-outputs map.
async fn merge_step_outputs(
  step: &ActionStep,
  stdout_outputs: HashMap<String, String>,
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> HashMap<String, String> {
  let mut outputs = apply_file_commands(file_cmds, ctx);
  // Record `$GITHUB_OUTPUT` file-command outputs into the context too, so
  // `${{ steps.<id>.outputs.* }}` (and job-level `outputs:`) can read them —
  // `apply_file_commands` only returns the map, it doesn't record it.
  for (key, value) in &outputs {
    ctx.set_step_output(&step.id, key, value);
  }
  for (key, value) in stdout_outputs {
    ctx.set_step_output(&step.id, &key, &value);
    outputs.insert(key, value);
  }
  outputs
}

/// Build the step's env map (global + step env + file-command paths + inherited
/// process env) and create the per-step file-command temp files.
fn build_step_env_and_file_commands(
  step: &ActionStep,
  ctx: &ExecutionContext,
  job: &JobCtx<'_>,
) -> Result<(HashMap<String, String>, FileCommandManager), RunnerError> {
  let step_env = resolve_step_env(step, ctx)?;
  let mut env = ctx.build_step_env(&step_env);
  let tmp_dir = job.config.data_dir.join("tmp");
  std::fs::create_dir_all(&tmp_dir)?;
  let (file_cmds, file_cmd_env) = FileCommandManager::create(&tmp_dir)?;
  env.extend(file_cmd_env);
  for (k, v) in std::env::vars() {
    env.entry(k).or_insert(v);
  }
  Ok((env, file_cmds))
}

/// Resolve a `run:` step's `working-directory` to an absolute child cwd.
///
/// The value arrives as the `workingDirectory` step input (the wire shape the
/// orchestrator emits for `working-directory:`). Precedence: step value >
/// job/workflow `defaults.run.working-directory` > workspace root. The chosen
/// value is `${{ }}`-interpolated, then a relative path is joined onto the
/// workspace and an absolute path is used as-is.
///
/// # Errors
///
/// Returns `RunnerError::Expression` if interpolation fails.
fn resolve_working_dir(
  step: &ActionStep,
  ctx: &ExecutionContext,
  workspace: &Path,
  defaults: &super::job_spec::RunDefaultsResolved,
) -> Result<PathBuf, RunnerError> {
  let raw = step
    .input("workingDirectory")
    .or_else(|| defaults.working_directory.clone());
  let Some(raw) = raw else {
    return Ok(workspace.to_path_buf());
  };
  let resolved = ctx.interpolate_string(&raw)?;
  if resolved.is_empty() {
    return Ok(workspace.to_path_buf());
  }
  let candidate = PathBuf::from(&resolved);
  if candidate.is_absolute() {
    Ok(candidate)
  } else {
    Ok(workspace.join(candidate))
  }
}

/// Split a step's real result from its effective conclusion. The outcome is
/// recorded by the caller; this returns the `continue-on-error`-adjusted
/// conclusion: a failed step with `continue-on-error: true` concludes as
/// `Success` so the job proceeds, while its `outcome` stays `Failure`.
fn apply_continue_on_error(
  step: &ActionStep,
  outcome: Conclusion,
  ctx: &mut ExecutionContext,
) -> Conclusion {
  let continue_on_error = step.continue_on_error.unwrap_or(false);
  if outcome == Conclusion::Failure && continue_on_error {
    return Conclusion::Success;
  }
  if outcome == Conclusion::Failure {
    ctx.record_step_failure();
  }
  outcome
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
