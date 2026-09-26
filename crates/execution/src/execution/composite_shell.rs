//! Bounded subprocess execution and parent-scoped logs for composite run steps.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use shared::{Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Parameters for running a composite-action shell script.
pub struct ShellScriptParams<'a> {
  /// Built-in shell or executable argument template containing `{0}`.
  pub shell: &'a str,
  /// Script body to run under that shell.
  pub script: &'a str,
  /// Environment variables to set on the child process.
  pub env: &'a HashMap<String, String>,
  /// Directory the script runs in.
  pub working_dir: &'a Path,
  /// Step id used to tag streamed log lines.
  pub log_step_id: &'a str,
  /// Per-job cgroup directory to move the spawned step into (`None` = no isolation).
  pub cgroup_path: Option<&'a Path>,
  /// Time left in the enclosing top-level step.
  pub timeout: Option<Duration>,
  /// Job cancellation token shared by the enclosing step.
  pub cancel: &'a CancellationToken,
}

/// Run a shell script as a subprocess, streaming output as log events.
///
/// # Errors
///
/// Returns `RunnerError` if the process cannot be spawned or waited on.
pub async fn run_shell_script(
  params: &ShellScriptParams<'_>,
  events: &mpsc::Sender<RunnerEvent>,
  stdout_tx: mpsc::Sender<String>,
) -> Result<Conclusion, RunnerError> {
  let output = super::handlers::script::ScriptHandler::new()
    .execute(
      &super::handlers::script::ScriptParams {
        script: params.script,
        shell: Some(params.shell),
        env: params.env,
        working_dir: params.working_dir,
        step_id: params.log_step_id,
        cgroup_path: params.cgroup_path,
        timeout: params.timeout,
        cancel: params.cancel,
        container: None,
      },
      events,
      stdout_tx,
    )
    .await?;
  Ok(output.conclusion)
}
