use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::execution::cgroup_join::spawn_in_cgroup;

/// Parameters for executing a Node.js action script.
pub struct NodeExecParams<'a> {
  pub node_binary: &'a Path,
  pub script_path: &'a Path,
  pub env: &'a HashMap<String, String>,
  pub working_dir: &'a Path,
  pub step_id: &'a str,
  /// Per-job cgroup directory to move the spawned step into (`None` = no isolation).
  pub cgroup_path: Option<&'a Path>,
}

/// Execute a Node.js action script.
///
/// Spawns `node {script_path}` with the given environment variables and working directory.
/// Stdout/stderr lines are emitted as `RunnerEvent::Log` events.
/// Exit code 0 -> Success, non-zero -> Failure.
///
/// # Errors
///
/// Returns `RunnerError` if the process cannot be spawned or waited on.
pub async fn execute_node_action(
  params: &NodeExecParams<'_>,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<Conclusion, RunnerError> {
  let mut cmd = tokio::process::Command::new(params.node_binary);
  cmd.arg(params.script_path);
  cmd.current_dir(params.working_dir);
  cmd.envs(std::env::vars());
  cmd.envs(params.env);
  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  let mut child = spawn_in_cgroup(&mut cmd, params.cgroup_path).await?;

  let stdout = child.stdout.take();
  let stderr = child.stderr.take();

  let stdout_handle = stream_output(stdout, params.step_id, LogStream::Stdout, events);
  let stderr_handle = stream_output(stderr, params.step_id, LogStream::Stderr, events);

  let status = child
    .wait()
    .await
    .map_err(|e| RunnerError::NodeHandler(format!("wait failed: {e}")))?;

  if let Some(h) = stdout_handle {
    let _ = h.await;
  }
  if let Some(h) = stderr_handle {
    let _ = h.await;
  }

  if status.success() {
    Ok(Conclusion::Success)
  } else {
    Ok(Conclusion::Failure)
  }
}

fn stream_output<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
  reader: Option<R>,
  step_id: &str,
  stream: LogStream,
  events: &mpsc::Sender<RunnerEvent>,
) -> Option<tokio::task::JoinHandle<()>> {
  let r = reader?;
  let tx = events.clone();
  let sid = step_id.to_owned();
  Some(tokio::spawn(async move {
    let buf = BufReader::new(r);
    let mut lines = buf.lines();
    while let Ok(Some(line)) = lines.next_line().await {
      let _ = tx
        .send(RunnerEvent::Log {
          step_id: sid.clone(),
          line,
          stream,
        })
        .await;
    }
  }))
}
