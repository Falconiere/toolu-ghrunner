//! Bounded subprocess execution and parent-scoped logs for composite run steps.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::cgroup_join::spawn_in_cgroup;
use super::step_timeout::{WaitOutcome, wait_bounded};

/// Parameters for running a composite-action shell script.
pub struct ShellScriptParams<'a> {
  /// Shell to invoke (`bash`, `sh`, `pwsh`, ...).
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
) -> Result<Conclusion, RunnerError> {
  let script_file = write_temp_script(params.script)?;
  let script_path = script_file.path().to_string_lossy().to_string();
  let (program, args) = shell_args(params.shell, &script_path);

  let mut cmd = tokio::process::Command::new(program);
  let mut env = params.env.clone();
  super::step_process_env::apply(&mut env, None);
  cmd
    .args(&args)
    .current_dir(params.working_dir)
    .env_clear()
    .envs(&env)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  #[cfg(unix)]
  cmd.process_group(0);

  let mut child = spawn_in_cgroup(&mut cmd, params.cgroup_path).await?;
  let group_id = child.id();

  let stdout = child.stdout.take();
  let stderr = child.stderr.take();

  let stdout_task = stream_output(stdout, params.log_step_id, LogStream::Stdout, events);
  let stderr_task = stream_output(stderr, params.log_step_id, LogStream::Stderr, events);

  let outcome = wait_bounded(&mut child, params.timeout, params.cancel, |message| {
    RunnerError::StepExecution(format!("composite {message}"))
  })
  .await;
  if matches!(&outcome, Ok(WaitOutcome::TimedOut | WaitOutcome::Cancelled)) {
    kill_process_group(group_id);
  }
  let ((), ()) = tokio::join!(finish_stream(stdout_task), finish_stream(stderr_task));
  match outcome? {
    WaitOutcome::Exited(status) if status.success() => Ok(Conclusion::Success),
    WaitOutcome::Exited(_) | WaitOutcome::TimedOut => Ok(Conclusion::Failure),
    WaitOutcome::Cancelled => Ok(Conclusion::Cancelled),
  }
}

#[cfg(unix)]
fn kill_process_group(group_id: Option<u32>) {
  use nix::errno::Errno;
  use nix::sys::signal::{Signal, killpg};
  use nix::unistd::Pid;

  let Some(pid) = group_id.and_then(|id| i32::try_from(id).ok()) else {
    return;
  };
  match killpg(Pid::from_raw(pid), Signal::SIGKILL) {
    Ok(()) | Err(Errno::ESRCH) => {},
    Err(error) => tracing::warn!(pid, %error, "failed to kill composite process group"),
  }
}

#[cfg(not(unix))]
fn kill_process_group(_group_id: Option<u32>) {}

fn write_temp_script(script: &str) -> Result<tempfile::NamedTempFile, RunnerError> {
  let mut file = tempfile::Builder::new()
    .suffix(".sh")
    .tempfile()
    .map_err(|e| RunnerError::StepExecution(format!("temp script: {e}")))?;
  std::io::Write::write_all(&mut file, script.as_bytes())
    .map_err(|e| RunnerError::StepExecution(format!("write script: {e}")))?;
  Ok(file)
}

fn shell_args(shell: &str, script_path: &str) -> (&'static str, Vec<String>) {
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
    _ => ("bash", vec!["-e".to_owned(), script_path.to_owned()]),
  }
}

fn stream_output<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
  reader: Option<R>,
  step_id: &str,
  stream: LogStream,
  events: &mpsc::Sender<RunnerEvent>,
) -> Option<JoinHandle<()>> {
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

async fn finish_stream(task: Option<JoinHandle<()>>) {
  let Some(mut task) = task else { return };
  match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
    Ok(Ok(())) => {},
    Ok(Err(error)) => tracing::warn!(%error, "composite output stream task failed"),
    Err(_) => {
      task.abort();
      tracing::warn!("composite output stream remained open after child exit");
    },
  }
}
