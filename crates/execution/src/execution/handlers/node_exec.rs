use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::execution::cgroup_join::spawn_in_cgroup;
use crate::execution::handlers::script::{
  bounded_drain, emit_timeout, forward_lines, stream_output,
};
use crate::execution::step_timeout::{WaitOutcome, wait_bounded};

/// Parameters for executing a Node.js action script.
pub struct NodeExecParams<'a> {
  pub node_binary: &'a Path,
  pub script_path: &'a Path,
  pub env: &'a HashMap<String, String>,
  pub working_dir: &'a Path,
  pub step_id: &'a str,
  /// Per-job cgroup directory to move the spawned step into (`None` = no isolation).
  pub cgroup_path: Option<&'a Path>,
  /// `timeout-minutes` bound for the child wait (`None` = unbounded).
  pub timeout: Option<Duration>,
  /// In-flight cancellation: a fired token kills the child mid-run.
  pub cancel: &'a CancellationToken,
}

/// Output of a Node.js action: its exit conclusion. Stdout is streamed to the
/// caller line-by-line during the run, not returned here.
pub struct NodeExecOutput {
  pub conclusion: Conclusion,
}

/// Execute a Node.js action script.
///
/// Spawns `node {script_path}` with the given environment and working
/// directory. Stdout is streamed line-by-line onto `stdout_tx` as the child
/// produces it, so the caller can dispatch workflow commands and emit `Log`
/// events in realtime (with `&mut ExecutionContext`). Stderr is streamed live
/// as `Log` events. Exit code 0 -> Success, non-zero -> Failure.
///
/// # Errors
///
/// Returns `RunnerError` if the process cannot be spawned or waited on.
pub async fn execute_node_action(
  params: &NodeExecParams<'_>,
  events: &mpsc::Sender<RunnerEvent>,
  stdout_tx: mpsc::Sender<String>,
) -> Result<NodeExecOutput, RunnerError> {
  let mut cmd = build_node_command(params);
  let mut child = spawn_in_cgroup(&mut cmd, params.cgroup_path).await?;

  let stdout_handle = forward_lines(child.stdout.take(), stdout_tx);
  let stderr_handle = stream_output(
    child.stderr.take(),
    params.step_id,
    LogStream::Stderr,
    events,
  );

  let outcome = wait_bounded(
    &mut child,
    params.timeout,
    params.cancel,
    RunnerError::NodeHandler,
  )
  .await?;

  // Bound the post-exit drain: a grandchild that inherited the pipe keeps it
  // open past the node child's exit, so abort the reader after the grace period
  // rather than block forever (mirrors the script handler).
  bounded_drain(stdout_handle).await;
  bounded_drain(stderr_handle).await;

  let conclusion = match outcome {
    WaitOutcome::Exited(status) if status.success() => Conclusion::Success,
    WaitOutcome::Exited(_) => Conclusion::Failure,
    WaitOutcome::TimedOut => {
      emit_timeout(events, params.step_id, params.timeout).await;
      Conclusion::Failure
    },
    WaitOutcome::Cancelled => Conclusion::Cancelled,
  };
  Ok(NodeExecOutput { conclusion })
}

/// Build the `node {script}` command with the step's cwd and environment.
fn build_node_command(params: &NodeExecParams<'_>) -> tokio::process::Command {
  let mut cmd = tokio::process::Command::new(params.node_binary);
  cmd.arg(params.script_path);
  cmd.current_dir(params.working_dir);
  // `params.env` (from `build_node_env`) is DELTA-only and relies on the
  // inherited process env for PATH/HOME, so inherit — but strip the runner's
  // private `TOOLU_RUNNER_*` namespace (incl. the admin re-mint bearer) first.
  cmd.envs(crate::execution::context::safe_process_env_vars());
  cmd.envs(params.env);
  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());
  cmd
}
