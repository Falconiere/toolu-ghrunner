//! Attached Docker exec with workflow environment confined to the container.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::models::ExecConfig;
use futures_util::{Stream, StreamExt};
use shared::{Conclusion, LogStream, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::job_container::JobContainer;

/// Arguments and bounds for one container process.
pub struct ContainerExec<'a> {
  /// Executable in host coordinates, or a command resolved by container PATH.
  pub program: &'a Path,
  /// Arguments already translated where they represent runner-owned paths.
  pub args: &'a [String],
  /// Explicit workflow environment, never applied to a host process.
  pub env: &'a HashMap<String, String>,
  /// Working directory in host or container coordinates.
  pub working_dir: &'a Path,
  /// Step identity used for log events.
  pub step_id: &'a str,
  /// Optional execution timeout.
  pub timeout: Option<Duration>,
  /// Job/step cancellation signal.
  pub cancel: &'a CancellationToken,
}

impl JobContainer {
  /// Execute and stream a process in this job container.
  ///
  /// A cancelled/timed-out connection is dropped; Docker has no kill-exec API.
  /// The job owner keeps this container for posts and then force-removes it.
  ///
  /// # Errors
  /// Returns a masked Docker error on create/start/stream/inspect failures.
  pub async fn execute(
    &self,
    params: &ContainerExec<'_>,
    events: &mpsc::Sender<RunnerEvent>,
    stdout: mpsc::Sender<String>,
  ) -> Result<Conclusion, RunnerError> {
    if params.cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }
    let deadline = async {
      match params.timeout {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending::<()>().await,
      }
    };
    tokio::select! {
      biased;
      () = params.cancel.cancelled() => Ok(Conclusion::Cancelled),
      () = deadline => {
        if events.send(RunnerEvent::Log {
          step_id: params.step_id.to_owned(),
          line: "##[error]Container step timed out; execution detached and remaining processes will be removed at job teardown.".to_owned(),
          stream: LogStream::Stderr,
        }).await.is_err() {
          tracing::warn!("job container timeout event receiver dropped; continuing");
        }
        Ok(Conclusion::Failure)
      },
      code = self.run_exec(params, events, &stdout) => {
        Ok(if code? == 0 { Conclusion::Success } else { Conclusion::Failure })
      }
    }
  }

  async fn run_exec(
    &self,
    params: &ContainerExec<'_>,
    events: &mpsc::Sender<RunnerEvent>,
    stdout: &mpsc::Sender<String>,
  ) -> Result<i64, RunnerError> {
    let exec = self
      .transport
      .docker
      .create_exec(self.id(), self.exec_config(params))
      .await
      .map_err(|e| self.transport.error("create job exec", e))?;
    let attached = self
      .transport
      .docker
      .start_exec(&exec.id, None)
      .await
      .map_err(|e| self.transport.error("start job exec", e))?;
    let StartExecResults::Attached {
      output,
      input: _input,
    } = attached
    else {
      return Err(RunnerError::Docker(
        "Docker did not attach job exec output".to_owned(),
      ));
    };
    let forward = forward_output(output, params.step_id, events, stdout);
    let wait = self.wait_exit(&exec.id);
    tokio::pin!(forward, wait);
    tokio::select! {
      result = &mut forward => {
        result.map_err(|e| self.transport.error("read job exec output", e))?;
        wait.await
      },
      result = &mut wait => {
        let code = result?;
        // Background descendants can keep the output pipe open after main exits.
        if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), forward).await {
          result.map_err(|e| self.transport.error("drain job exec output", e))?;
        }
        Ok(code)
      }
    }
  }

  fn exec_config(&self, params: &ContainerExec<'_>) -> ExecConfig {
    let mut command = vec![
      self
        .translator()
        .to_container(params.program)
        .to_string_lossy()
        .into_owned(),
    ];
    command.extend_from_slice(params.args);
    let mut env = params.env.clone();
    crate::execution::step_process_env::apply(&mut env, Some(&self.base_ci));
    ExecConfig {
      cmd: Some(command),
      env: Some(
        env
          .iter()
          .map(|(key, value)| format!("{key}={}", self.translate_env(key, value)))
          .collect(),
      ),
      working_dir: Some(
        self
          .translator()
          .to_container(params.working_dir)
          .to_string_lossy()
          .into_owned(),
      ),
      attach_stdout: Some(true),
      attach_stderr: Some(true),
      ..Default::default()
    }
  }

  async fn wait_exit(&self, exec: &str) -> Result<i64, RunnerError> {
    loop {
      let inspect = self
        .transport
        .docker
        .inspect_exec(exec)
        .await
        .map_err(|e| self.transport.error("inspect job exec", e))?;
      if inspect.running == Some(false) {
        return inspect
          .exit_code
          .ok_or_else(|| RunnerError::Docker("Docker exec exited without a status".to_owned()));
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  }
}

async fn forward_output(
  mut output: impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin,
  step: &str,
  events: &mpsc::Sender<RunnerEvent>,
  stdout: &mpsc::Sender<String>,
) -> Result<(), bollard::errors::Error> {
  let mut out = Vec::new();
  let mut err = Vec::new();
  let mut stderr_events_open = true;
  let mut stdout_open = true;
  while let Some(item) = output.next().await {
    let (buffer, message, stderr) = match item? {
      LogOutput::StdOut { message } | LogOutput::Console { message } => (&mut out, message, false),
      LogOutput::StdErr { message } => (&mut err, message, true),
      LogOutput::StdIn { .. } => continue,
    };
    buffer.extend_from_slice(&message);
    while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
      let mut bytes: Vec<_> = buffer.drain(..=end).collect();
      bytes.pop();
      if bytes.last() == Some(&b'\r') {
        bytes.pop();
      }
      let open = if stderr {
        &mut stderr_events_open
      } else {
        &mut stdout_open
      };
      forward_line(&bytes, stderr, step, events, stdout, open).await;
    }
  }
  if !out.is_empty() {
    forward_line(&out, false, step, events, stdout, &mut stdout_open).await;
  }
  if !err.is_empty() {
    forward_line(&err, true, step, events, stdout, &mut stderr_events_open).await;
  }
  Ok(())
}

async fn forward_line(
  bytes: &[u8],
  stderr: bool,
  step: &str,
  events: &mpsc::Sender<RunnerEvent>,
  stdout: &mpsc::Sender<String>,
  open: &mut bool,
) {
  if !*open {
    return;
  }
  // Both channels carry text logs/workflow commands, not binary output. Replace
  // invalid UTF-8 with U+FFFD so arbitrary process bytes do not fail the job.
  let line = String::from_utf8_lossy(bytes).into_owned();
  let closed = if stderr {
    events
      .send(RunnerEvent::Log {
        step_id: step.to_owned(),
        line,
        stream: LogStream::Stderr,
      })
      .await
      .is_err()
  } else {
    stdout.send(line).await.is_err()
  };
  if !closed {
    return;
  }
  *open = false;
  if stderr {
    tracing::warn!("job container stderr event receiver dropped; continuing");
  } else {
    tracing::warn!("job container stdout receiver dropped; continuing");
  }
}
