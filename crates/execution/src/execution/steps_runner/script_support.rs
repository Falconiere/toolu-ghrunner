//! Script-step environment setup, stdout dispatch, and output merging.

use std::collections::HashMap;

use shared::{ActionStep, Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;

use super::super::command_dispatch::stream_dispatch_stdout;
use super::super::context::ExecutionContext;
use super::super::file_commands::{FileCommandManager, create_file_command_dir};
use super::super::handlers::script::{ScriptHandler, ScriptParams};
use super::super::step_env::apply_file_commands_and_merge_outputs;
use super::JobCtx;

/// Run the shell child and stream-dispatch its stdout concurrently.
///
/// The handler forwards each stdout line onto `stdout_tx` as it is read, while
/// `stream_dispatch_stdout` (owning `&mut ctx`) dispatches commands and emits
/// `Log` events in realtime. `execute` borrows only params/events/sender, so
/// it does not conflict with the concurrent `&mut ctx` consumer. Returns the
/// exit conclusion and the `set-output` map.
pub(super) async fn run_and_dispatch_script(
  handler: &ScriptHandler,
  params: &ScriptParams<'_>,
  step: &ActionStep,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(256);
  // `execute` takes the sender by value and owns the only producer copy; when
  // its future completes (after EOF) the sender drops, so the dispatcher's
  // `recv` sees channel close and returns its `set-output` map.
  let exec = handler.execute(params, events, stdout_tx);
  let dispatch = stream_dispatch_stdout(
    &step.id,
    &step.id,
    step.expression_name(),
    &mut stdout_rx,
    ctx,
    events,
  );
  let (exec_result, stdout_outputs) = tokio::join!(exec, dispatch);
  Ok((exec_result?.conclusion, stdout_outputs))
}

/// Merge a script step's stdout `set-output` outputs (already applied to `ctx`
/// by the streaming dispatcher) with its `$GITHUB_OUTPUT` file-command outputs
/// into one step-outputs map. Thin wrapper over the shared
/// [`apply_file_commands_and_merge_outputs`], which also applies the step's
/// `$GITHUB_ENV`/`$GITHUB_PATH` file commands to `ctx`.
pub(super) async fn merge_step_outputs(
  step: &ActionStep,
  stdout_outputs: HashMap<String, String>,
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> HashMap<String, String> {
  apply_file_commands_and_merge_outputs(
    &step.id,
    step.expression_name(),
    None,
    stdout_outputs,
    file_cmds,
    ctx,
  )
  .await
}

/// Build process env from the caller's already-resolved step overlay and job
/// env, then add inherited env and new per-step file-command paths. Synchronous
/// filesystem work is batched onto Tokio's blocking pool by the shared
/// file-command helpers.
pub(super) async fn build_step_env_and_file_commands(
  ctx: &ExecutionContext,
  job: &JobCtx<'_>,
) -> Result<(HashMap<String, String>, FileCommandManager), RunnerError> {
  let mut env = ctx.build_step_env(&HashMap::new());
  let tmp_dir = job.config.data_dir.join("tmp");
  create_file_command_dir(&tmp_dir).await?;
  let (file_cmds, file_cmd_env) = FileCommandManager::create(&tmp_dir).await?;
  env.extend(file_cmd_env);
  // Fold the process env LAST (lowest precedence), stripping the runner's
  // private `TOOLU_RUNNER_*` namespace so the admin re-mint bearer never
  // reaches the step child.
  if ctx.job_container().is_none() {
    for (k, v) in super::super::context::safe_process_env_vars() {
      env.entry(k).or_insert(v);
    }
  }
  Ok((env, file_cmds))
}
