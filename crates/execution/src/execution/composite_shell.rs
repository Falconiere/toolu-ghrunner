use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use super::cgroup_join::spawn_in_cgroup;

/// Parameters for running a composite-action shell script.
pub struct ShellScriptParams<'a> {
  pub shell: &'a str,
  pub script: &'a str,
  pub env: &'a HashMap<String, String>,
  pub working_dir: &'a Path,
  pub log_step_id: &'a str,
  /// Per-job cgroup directory to move the spawned step into (`None` = no isolation).
  pub cgroup_path: Option<&'a Path>,
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
  cmd
    .args(&args)
    .current_dir(params.working_dir)
    .env_clear()
    .envs(params.env)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

  let mut child = spawn_in_cgroup(&mut cmd, params.cgroup_path).await?;

  let stdout = child.stdout.take();
  let stderr = child.stderr.take();

  stream_output(stdout, params.log_step_id, LogStream::Stdout, events);
  stream_output(stderr, params.log_step_id, LogStream::Stderr, events);

  let status = child
    .wait()
    .await
    .map_err(|e| RunnerError::StepExecution(format!("composite wait failed: {e}")))?;

  if status.success() {
    Ok(Conclusion::Success)
  } else {
    Ok(Conclusion::Failure)
  }
}

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
) {
  let Some(r) = reader else { return };
  let tx = events.clone();
  let sid = step_id.to_owned();
  tokio::spawn(async move {
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
  });
}
