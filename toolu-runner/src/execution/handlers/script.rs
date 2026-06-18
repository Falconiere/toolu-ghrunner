use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::execution::cgroup_join::spawn_in_cgroup;

/// Parameters for script execution.
pub struct ScriptParams<'a> {
  pub script: &'a str,
  pub shell: Option<&'a str>,
  pub env: &'a HashMap<String, String>,
  pub working_dir: &'a Path,
  pub step_id: &'a str,
  /// Per-job cgroup directory to move the spawned step into (`None` = no isolation).
  pub cgroup_path: Option<&'a Path>,
}

/// Executes `run:` step scripts as shell processes.
pub struct ScriptHandler;

impl ScriptHandler {
  pub fn new() -> Self {
    Self
  }

  /// Execute a script string in a shell process.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::ScriptHandler` if the process cannot be spawned.
  pub async fn execute(
    &self,
    params: &ScriptParams<'_>,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Result<Conclusion, RunnerError> {
    let shell_name = params.shell.unwrap_or("bash");
    let script_file = write_script_file(params.script, shell_name)?;
    let script_path = script_file.path().to_string_lossy().to_string();

    let (program, args) = build_shell_args(shell_name, &script_path);

    let mut cmd = tokio::process::Command::new(program);
    cmd
      .args(&args)
      .current_dir(params.working_dir)
      .envs(std::env::vars())
      .envs(params.env)
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());

    let mut child = spawn_in_cgroup(&mut cmd, params.cgroup_path).await?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = stream_output(stdout, params.step_id, LogStream::Stdout, events);
    let stderr_handle = stream_output(stderr, params.step_id, LogStream::Stderr, events);

    let status = child
      .wait()
      .await
      .map_err(|e| RunnerError::ScriptHandler(format!("wait failed: {e}")))?;

    // Wait for output tasks to finish before returning — ensures all log
    // lines are emitted before StepCompleted.
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
