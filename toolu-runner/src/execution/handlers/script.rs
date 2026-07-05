use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::execution::cgroup_join::spawn_in_cgroup;
use crate::execution::step_timeout::{WaitOutcome, wait_bounded};

/// Parameters for script execution.
pub struct ScriptParams<'a> {
  pub script: &'a str,
  pub shell: Option<&'a str>,
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

/// Executes `run:` step scripts as shell processes.
pub struct ScriptHandler;

impl ScriptHandler {
  /// Create a new script handler.
  pub fn new() -> Self {
    Self
  }

  /// Execute a script string in a shell process.
  ///
  /// Stdout is streamed line-by-line onto `stdout_tx` as the child produces it,
  /// so the caller can dispatch workflow commands and emit `Log` events in
  /// realtime (with `&mut ExecutionContext`). Stderr is streamed live as `Log`
  /// events. Returns the exit conclusion.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::ScriptHandler` if the process cannot be spawned.
  pub async fn execute(
    &self,
    params: &ScriptParams<'_>,
    events: &mpsc::Sender<RunnerEvent>,
    stdout_tx: mpsc::Sender<String>,
  ) -> Result<ScriptOutput, RunnerError> {
    let shell_name = params.shell.unwrap_or("bash");
    let script_file = write_script_file(params.script, shell_name)?;
    let script_path = script_file.path().to_string_lossy().to_string();

    let mut child = spawn_step_shell(params, shell_name, &script_path).await?;

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
      RunnerError::ScriptHandler,
    )
    .await?;
    let conclusion = match outcome {
      WaitOutcome::Exited(status) => conclusion_for(status.success()),
      WaitOutcome::TimedOut => {
        emit_timeout(events, params.step_id, params.timeout).await;
        Conclusion::Failure
      },
      WaitOutcome::Cancelled => Conclusion::Cancelled,
    };

    finish_streams(stdout_handle, stderr_handle).await;
    Ok(ScriptOutput { conclusion })
  }
}

/// Grace period to drain already-buffered output AFTER the child exits, before
/// aborting a reader that is still blocked on the pipe. A grandchild that
/// inherited the pipe (e.g. a backgrounded process, a build-script / rustc
/// proc-macro server) keeps it open past the child's exit, so the read never
/// hits EOF; once this elapses we abort the reader so its `Sender` drops and
/// the downstream `recv()` loop closes, completing the step.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Emit the standard "timed out" log line so the cause is visible in the UI.
///
/// Shared with the node-action handler (`handlers::node_exec`) so both child
/// runners emit an identical timeout line.
pub(crate) async fn emit_timeout(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  timeout: Option<Duration>,
) {
  let secs = timeout.map(|d| d.as_secs()).unwrap_or(0);
  let _ = events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: format!("##[error]The step exceeded its timeout of {secs}s and was terminated."),
      stream: LogStream::Stderr,
    })
    .await;
}

/// Settle the live stdout/stderr forwarders before the caller reports
/// `StepCompleted`. Each forwarder is given a bounded grace period to finish
/// reading already-buffered lines; if it is still blocked on the pipe (a
/// grandchild holds it open past the child's exit), it is aborted so its
/// `Sender` drops, closing the downstream channel and unblocking the step.
async fn finish_streams(
  stdout_handle: Option<tokio::task::JoinHandle<()>>,
  stderr_handle: Option<tokio::task::JoinHandle<()>>,
) {
  bounded_drain(stdout_handle).await;
  bounded_drain(stderr_handle).await;
}

/// Wait up to [`DRAIN_GRACE`] for a reader task to finish, then abort it.
///
/// Aborting (not merely dropping the `JoinHandle`) is what frees the task's
/// `Sender`: a dropped handle leaves the task running, so the channel would
/// never close. The grace period lets a well-behaved child's
/// already-written-but-unread lines (e.g. `::set-output::`) drain first.
pub(crate) async fn bounded_drain(handle: Option<tokio::task::JoinHandle<()>>) {
  let Some(mut h) = handle else { return };
  // Borrow the handle so it survives a timeout and can still be aborted.
  if tokio::time::timeout(DRAIN_GRACE, &mut h).await.is_err() {
    h.abort();
    // Reap the aborted task so its `Sender` is dropped before we return.
    let _ = h.await;
  }
}

fn conclusion_for(success: bool) -> Conclusion {
  if success {
    Conclusion::Success
  } else {
    Conclusion::Failure
  }
}

/// Result of running a script step: its exit conclusion. Stdout is streamed to
/// the caller line-by-line during the run, not returned here.
pub struct ScriptOutput {
  pub conclusion: Conclusion,
}

impl Default for ScriptHandler {
  fn default() -> Self {
    Self::new()
  }
}

fn write_script_file(script: &str, shell: &str) -> Result<tempfile::NamedTempFile, RunnerError> {
  let suffix = match shell {
    "python" | "python3" => ".py",
    "pwsh" | "powershell" => ".ps1",
    _ => ".sh",
  };
  let mut file = tempfile::Builder::new()
    .suffix(suffix)
    .tempfile()
    .map_err(|e| RunnerError::ScriptHandler(format!("temp file: {e}")))?;
  std::io::Write::write_all(&mut file, script.as_bytes())
    .map_err(|e| RunnerError::ScriptHandler(format!("write script: {e}")))?;
  Ok(file)
}

/// Build the shell command for a step script and spawn it, mapping a
/// spawn failure to a `ScriptHandler` error that names the program and
/// working directory (a bare `Io` ENOENT is undiagnosable — it names
/// neither the missing executable nor the missing cwd).
async fn spawn_step_shell(
  params: &ScriptParams<'_>,
  shell_name: &str,
  script_path: &str,
) -> Result<tokio::process::Child, RunnerError> {
  let (program, args) = build_shell_args(shell_name, script_path);
  let mut cmd = tokio::process::Command::new(program);
  cmd
    .args(&args)
    .current_dir(params.working_dir)
    .envs(std::env::vars())
    .envs(params.env)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  spawn_in_cgroup(&mut cmd, params.cgroup_path)
    .await
    .map_err(|e| {
      RunnerError::ScriptHandler(format!(
        "spawning step shell '{program}' (cwd: {}): {e}",
        params.working_dir.display()
      ))
    })
}

fn build_shell_args(shell: &str, script_path: &str) -> (&'static str, Vec<String>) {
  match shell {
    "bash" => (
      "bash",
      vec![
        "--noprofile".to_owned(),
        "--norc".to_owned(),
        "-e".to_owned(),
        "-o".to_owned(),
        "pipefail".to_owned(),
        script_path.to_owned(),
      ],
    ),
    "sh" => ("sh", vec!["-e".to_owned(), script_path.to_owned()]),
    "python" | "python3" => ("python3", vec![script_path.to_owned()]),
    "pwsh" => (
      "pwsh",
      vec!["-command".to_owned(), format!(". '{script_path}'")],
    ),
    _ => ("bash", vec!["-e".to_owned(), script_path.to_owned()]),
  }
}

/// Spawn a task that forwards every line read from `reader` as a `Log` event
/// on `stream`. Returns `None` when the child gave no pipe for that stream.
///
/// Shared with the node-action handler (`handlers::node_exec`) for streaming
/// a child's stderr.
pub(crate) fn stream_output<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
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

/// Forward every stdout line onto `tx` as the child produces it, so the caller
/// can dispatch workflow commands and emit `Log` events in realtime (the
/// consumer owns `&mut ExecutionContext`, unavailable inside this task). The
/// channel closes when the child's stdout reaches EOF.
pub(crate) fn forward_lines<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
  reader: Option<R>,
  tx: mpsc::Sender<String>,
) -> Option<tokio::task::JoinHandle<()>> {
  let r = reader?;
  Some(tokio::spawn(async move {
    let buf = BufReader::new(r);
    let mut lines = buf.lines();
    while let Ok(Some(line)) = lines.next_line().await {
      if tx.send(line).await.is_err() {
        break;
      }
    }
  }))
}
