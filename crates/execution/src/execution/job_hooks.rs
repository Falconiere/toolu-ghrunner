//! Self-hosted job hooks: `ACTIONS_RUNNER_HOOK_JOB_STARTED` runs before the
//! first step, `ACTIONS_RUNNER_HOOK_JOB_COMPLETED` runs after post-drain.
//!
//! Each env var, when set, names a script path the runner executes around the
//! job. The job-started hook is a hard gate — its failure fails the job. The
//! job-completed hook runs best-effort: a non-zero exit is logged but does not
//! change the job conclusion (matching the C# runner contract). Hooks inherit
//! the job env so they observe `GITHUB_*` / `RUNNER_*` like steps do.

use std::collections::HashMap;
use std::path::Path;

use shared::{Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::context::ExecutionContext;
use super::handlers::script::{ScriptHandler, ScriptParams};

/// Which job boundary a hook runs at.
#[derive(Debug, Clone, Copy)]
pub enum JobHookStage {
  Started,
  Completed,
}

impl JobHookStage {
  /// The env var that, when set, names the hook script for this stage.
  fn env_var(self) -> &'static str {
    match self {
      Self::Started => "ACTIONS_RUNNER_HOOK_JOB_STARTED",
      Self::Completed => "ACTIONS_RUNNER_HOOK_JOB_COMPLETED",
    }
  }

  /// Synthetic step id used for the hook's log/started events.
  fn step_id(self) -> &'static str {
    match self {
      Self::Started => "__job_started_hook",
      Self::Completed => "__job_completed_hook",
    }
  }

  /// Human-readable label shown in the log group header.
  fn label(self) -> &'static str {
    match self {
      Self::Started => "Job started hook",
      Self::Completed => "Job completed hook",
    }
  }
}

/// Run the hook for `stage` if its env var is set, against the job env in `ctx`.
///
/// Returns the hook's conclusion when it ran, or `None` when no hook is
/// configured (the common case). The caller decides how to propagate a
/// `Failure`: the job-started hook fails the job, the job-completed hook is
/// best-effort.
///
/// # Errors
///
/// Returns `RunnerError::ScriptHandler` if the hook process cannot be spawned.
pub async fn run_job_hook(
  stage: JobHookStage,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  workspace: &Path,
  cancel: &CancellationToken,
) -> Result<Option<Conclusion>, RunnerError> {
  let Some(script_path) = ctx.env_var(stage.env_var()) else {
    return Ok(None);
  };
  if script_path.is_empty() {
    return Ok(None);
  }

  let env = job_hook_env(ctx);
  let shell = detect_shell(&script_path);
  // Run the hook's OWN source under the interpreter its extension selects —
  // a `.py` hook must be executed by `python3`, not dot-sourced into a bash
  // wrapper (which a non-bash interpreter would choke on). Read the file and
  // hand its content to the same handler steps use.
  let script = std::fs::read_to_string(&script_path)
    .map_err(|e| RunnerError::ScriptHandler(format!("read hook script '{script_path}': {e}")))?;

  // Group header carries the label; the hook's script path is echoed inside
  // the group (mirroring the step path, where the command echo is grouped and
  // the script's own output follows ungrouped).
  emit_log(
    events,
    stage.step_id(),
    &format!("##[group]{}", stage.label()),
  )
  .await;
  emit_log(events, stage.step_id(), &script_path).await;
  emit_log(events, stage.step_id(), "##[endgroup]").await;

  let params = ScriptParams {
    script: &script,
    shell: Some(shell),
    env: &env,
    working_dir: workspace,
    step_id: stage.step_id(),
    cgroup_path: ctx.cgroup_path(),
    timeout: None,
    cancel,
  };

  let conclusion = run_hook_script(&params, stage.step_id(), events).await?;
  Ok(Some(conclusion))
}

/// Run a hook script, streaming its stdout as plain `Log` events line-by-line
/// (hooks do not dispatch workflow commands). `execute` owns the only producer
/// copy of the channel, so the drain ends when the child EOFs.
async fn run_hook_script(
  params: &ScriptParams<'_>,
  step_id: &str,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<Conclusion, RunnerError> {
  let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(256);
  let handler = ScriptHandler::new();
  let exec = handler.execute(params, events, stdout_tx);
  let drain = async {
    while let Some(line) = stdout_rx.recv().await {
      emit_stdout_line(events, step_id, line).await;
    }
  };
  let (output, ()) = tokio::join!(exec, drain);
  Ok(output?.conclusion)
}

/// Forward one hook stdout line as a plain stdout `Log` event.
async fn emit_stdout_line(events: &mpsc::Sender<RunnerEvent>, step_id: &str, line: String) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line,
      stream: shared::LogStream::Stdout,
    })
    .await;
}

/// Pick an interpreter from the hook script's extension; default to `bash`.
fn detect_shell(script_path: &str) -> &'static str {
  match Path::new(script_path).extension().and_then(|e| e.to_str()) {
    Some("ps1") => "pwsh",
    Some("py") => "python3",
    _ => "bash",
  }
}

/// Build the hook's env from the job env plus the inherited process env, so a
/// hook sees the same `GITHUB_*` / `RUNNER_*` a step would.
fn job_hook_env(ctx: &ExecutionContext) -> HashMap<String, String> {
  let mut env = ctx.build_step_env(&HashMap::new());
  // Strip the runner's private `TOOLU_RUNNER_*` namespace (incl. the admin
  // re-mint bearer) from the inherited process env before it reaches the hook.
  for (k, v) in super::context::safe_process_env_vars() {
    env.entry(k).or_insert(v);
  }
  env
}

async fn emit_log(events: &mpsc::Sender<RunnerEvent>, step_id: &str, line: &str) {
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: line.to_owned(),
      stream: shared::LogStream::Stdout,
    })
    .await;
}
