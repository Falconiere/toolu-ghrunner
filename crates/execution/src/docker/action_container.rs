//! One owned Docker container per container-action stage.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::container::{AttachContainerResults, LogOutput};
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
  AttachContainerOptionsBuilder, CreateContainerOptions, RemoveContainerOptions,
  StartContainerOptions, WaitContainerOptions,
};
use futures_util::{Stream, StreamExt};
use shared::{Conclusion, LogStream, RunnerConfig, RunnerError, RunnerEvent, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_mounts::ActionMounts;
use super::container_command::ContainerCommand;

/// Inputs for one owned container-action stage.
pub(crate) struct ActionContainerParams<'a> {
  /// Prepared image reference.
  pub(crate) image: &'a str,
  /// Optional entrypoint override.
  pub(crate) entrypoint: Option<&'a str>,
  /// Optional argv override, preserving absent versus empty.
  pub(crate) args: Option<&'a [String]>,
  /// Evaluated action environment.
  pub(crate) env: &'a HashMap<String, String>,
  /// Directories added through prior `GITHUB_PATH` commands.
  pub(crate) path_additions: &'a [String],
  /// Runner directory configuration.
  pub(crate) config: &'a RunnerConfig,
  /// Current job workspace on the host.
  pub(crate) workspace: &'a Path,
  /// Resolved action source directory on the host.
  pub(crate) action_dir: &'a Path,
  /// Existing job network, when the job owns one.
  pub(crate) network: Option<&'a str>,
  /// Step id attached to output events and ownership labels.
  pub(crate) step_id: &'a str,
  /// Remaining stage timeout measured from `run` entry.
  pub(crate) timeout: Option<Duration>,
  /// Job and step cancellation signal.
  pub(crate) cancel: &'a CancellationToken,
}

/// Docker transport used to prepare images and run isolated action stages.
pub(crate) struct ActionContainer {
  pub(super) transport: ContainerCommand,
  docker_socket: std::path::PathBuf,
}

impl ActionContainer {
  /// Connect to the configured local Linux Docker daemon.
  pub(crate) async fn connect(masker: Arc<Mutex<SecretMasker>>) -> Result<Self, RunnerError> {
    let transport = ContainerCommand::connect(masker).await?;
    let docker_socket = transport.socket_path().to_path_buf();
    Ok(Self {
      transport,
      docker_socket,
    })
  }

  /// Run one stage, stream its output, and remove its owned container.
  pub(crate) async fn run(
    &self,
    params: &ActionContainerParams<'_>,
    events: &mpsc::Sender<RunnerEvent>,
    stdout_sender: mpsc::Sender<String>,
  ) -> Result<Conclusion, RunnerError> {
    let deadline = params
      .timeout
      .map(|duration| tokio::time::Instant::now() + duration);
    if params.cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }
    let mounts = materialize_mounts(params, &self.docker_socket).await?;
    if params.cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }
    if deadline_elapsed(deadline) {
      send_timeout(events, params.step_id).await;
      return Ok(Conclusion::Failure);
    }
    let body = self.create_body(params, &mounts).await?;
    if deadline_elapsed(deadline) {
      send_timeout(events, params.step_id).await;
      return Ok(Conclusion::Failure);
    }
    let created = self
      .transport
      .docker
      .create_container(None::<CreateContainerOptions>, body)
      .await
      .map_err(|error| {
        self
          .transport
          .error("create Docker action container", error)
      })?;
    let result = self
      .run_created(&created.id, params, deadline, events, stdout_sender)
      .await;
    let cleanup = self.remove(&created.id).await;
    combine_result(result, cleanup)
  }

  async fn run_created(
    &self,
    id: &str,
    params: &ActionContainerParams<'_>,
    deadline: Option<tokio::time::Instant>,
    events: &mpsc::Sender<RunnerEvent>,
    stdout: mpsc::Sender<String>,
  ) -> Result<Conclusion, RunnerError> {
    if params.cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }
    let output = self.attach(id).await?;
    if params.cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }
    if deadline_elapsed(deadline) {
      send_timeout(events, params.step_id).await;
      return Ok(Conclusion::Failure);
    }
    self.start(id).await?;
    if params.cancel.is_cancelled() {
      return Ok(Conclusion::Cancelled);
    }
    if deadline_elapsed(deadline) {
      send_timeout(events, params.step_id).await;
      return Ok(Conclusion::Failure);
    }
    tokio::select! {
      biased;
      () = params.cancel.cancelled() => Ok(Conclusion::Cancelled),
      () = wait_for_deadline(deadline) => {
        send_timeout(events, params.step_id).await;
        Ok(Conclusion::Failure)
      },
      status = self.drive(id, output, params.step_id, events, &stdout) => {
        Ok(if status? == 0 { Conclusion::Success } else { Conclusion::Failure })
      }
    }
  }

  async fn attach(
    &self,
    id: &str,
  ) -> Result<impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin, RunnerError>
  {
    let AttachContainerResults {
      output,
      input: _input,
    } = self
      .transport
      .docker
      .attach_container(
        id,
        Some(
          AttachContainerOptionsBuilder::default()
            .stream(true)
            .stdout(true)
            .stderr(true)
            .build(),
        ),
      )
      .await
      .map_err(|error| self.transport.error("attach Docker action output", error))?;
    Ok(output)
  }

  async fn start(&self, id: &str) -> Result<(), RunnerError> {
    self
      .transport
      .docker
      .start_container(id, None::<StartContainerOptions>)
      .await
      .map_err(|error| self.transport.error("start Docker action container", error))
  }
}

impl ActionContainer {
  async fn drive(
    &self,
    id: &str,
    output: impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin,
    step_id: &str,
    events: &mpsc::Sender<RunnerEvent>,
    stdout: &mpsc::Sender<String>,
  ) -> Result<i64, RunnerError> {
    let forward = forward_output(output, step_id, events, stdout);
    let wait = self.wait(id);
    tokio::pin!(forward, wait);
    tokio::select! {
      result = &mut forward => {
        result.map_err(|error| self.transport.error("read Docker action output", error))?;
        wait.await
      },
      result = &mut wait => {
        let code = result?;
        if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), forward).await {
          result.map_err(|error| self.transport.error("drain Docker action output", error))?;
        }
        Ok(code)
      }
    }
  }

  async fn wait(&self, id: &str) -> Result<i64, RunnerError> {
    let mut stream = self.transport.docker.wait_container(
      id,
      Some(WaitContainerOptions {
        condition: "not-running".to_owned(),
      }),
    );
    stream
      .next()
      .await
      .ok_or_else(|| RunnerError::Docker("Docker action wait ended without a status".to_owned()))?
      .map(|result| result.status_code)
      .map_err(|error| {
        self
          .transport
          .error("wait for Docker action container", error)
      })
  }

  async fn create_body(
    &self,
    params: &ActionContainerParams<'_>,
    mounts: &ActionMounts,
  ) -> Result<ContainerCreateBody, RunnerError> {
    let inspected = self
      .transport
      .docker
      .inspect_image(params.image)
      .await
      .map_err(|error| self.transport.error("inspect Docker action image", error))?;
    let image_path = inspected
      .config
      .and_then(|config| config.env)
      .unwrap_or_default()
      .into_iter()
      .find_map(|value| value.strip_prefix("PATH=").map(str::to_owned));
    let env = build_env(params, mounts, image_path.as_deref());
    Ok(ContainerCreateBody {
      image: Some(params.image.to_owned()),
      entrypoint: params.entrypoint.map(|value| vec![value.to_owned()]),
      cmd: params.args.map(<[String]>::to_vec),
      env: Some(env),
      working_dir: Some("/github/workspace".to_owned()),
      attach_stdout: Some(true),
      attach_stderr: Some(true),
      labels: Some(HashMap::from([
        ("io.toolu.action-container".to_owned(), "true".to_owned()),
        ("io.toolu.action-step".to_owned(), params.step_id.to_owned()),
      ])),
      host_config: Some(HostConfig {
        binds: Some(mounts.volumes.clone()),
        network_mode: params.network.map(str::to_owned),
        ..Default::default()
      }),
      ..Default::default()
    })
  }

  async fn remove(&self, id: &str) -> Result<(), RunnerError> {
    match self
      .transport
      .docker
      .remove_container(
        id,
        Some(RemoveContainerOptions {
          force: true,
          ..Default::default()
        }),
      )
      .await
    {
      Ok(())
      | Err(bollard::errors::Error::DockerResponseServerError {
        status_code: 404, ..
      }) => Ok(()),
      Err(error) => Err(
        self
          .transport
          .error("remove Docker action container", error),
      ),
    }
  }
}

fn build_env(
  params: &ActionContainerParams<'_>,
  mounts: &ActionMounts,
  image_path: Option<&str>,
) -> Vec<String> {
  let mut env = params.env.clone();
  crate::execution::step_process_env::apply(&mut env, None);
  let base_path = env
    .remove("PATH")
    .or_else(|| image_path.map(str::to_owned))
    .unwrap_or_default();
  let mut path = params
    .path_additions
    .iter()
    .rev()
    .map(|value| {
      mounts
        .translator
        .to_container(Path::new(value))
        .to_string_lossy()
        .into_owned()
    })
    .collect::<Vec<_>>();
  if !base_path.is_empty() {
    path.push(mounts.translate_env("PATH", &base_path));
  }
  if !path.is_empty() {
    env.insert("PATH".to_owned(), path.join(":"));
  }
  env.insert("HOME".to_owned(), "/github/home".to_owned());
  let mut result: Vec<_> = env
    .iter()
    .map(|(key, value)| format!("{key}={}", mounts.translate_env(key, value)))
    .collect();
  result.sort();
  result
}

async fn materialize_mounts(
  params: &ActionContainerParams<'_>,
  docker_socket: &Path,
) -> Result<ActionMounts, RunnerError> {
  let config = params.config.clone();
  let workspace = params.workspace.to_path_buf();
  let action_dir = params.action_dir.to_path_buf();
  let docker_socket = docker_socket.to_path_buf();
  tokio::task::spawn_blocking(move || {
    ActionMounts::create(&config, &workspace, &action_dir, &docker_socket)
  })
  .await
  .map_err(|error| RunnerError::Docker(format!("materialize Docker action mounts: {error}")))?
}

fn combine_result(
  result: Result<Conclusion, RunnerError>,
  cleanup: Result<(), RunnerError>,
) -> Result<Conclusion, RunnerError> {
  match (result, cleanup) {
    (Ok(conclusion), Ok(())) => Ok(conclusion),
    (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
    (Err(error), Err(cleanup)) => Err(RunnerError::Docker(format!(
      "{error}; cleanup also failed: {cleanup}"
    ))),
  }
}

async fn send_timeout(events: &mpsc::Sender<RunnerEvent>, step_id: &str) {
  if events
    .send(RunnerEvent::Log {
      step_id: step_id.to_owned(),
      line: "##[error]Docker action timed out; removing its container.".to_owned(),
      stream: LogStream::Stderr,
    })
    .await
    .is_err()
  {
    tracing::warn!("Docker action timeout event receiver dropped; continuing");
  }
}

fn deadline_elapsed(deadline: Option<tokio::time::Instant>) -> bool {
  deadline.is_some_and(|value| value <= tokio::time::Instant::now())
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
  match deadline {
    Some(value) => tokio::time::sleep_until(value).await,
    None => std::future::pending::<()>().await,
  }
}

async fn forward_output(
  mut output: impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin,
  step_id: &str,
  events: &mpsc::Sender<RunnerEvent>,
  stdout: &mpsc::Sender<String>,
) -> Result<(), bollard::errors::Error> {
  let mut out = Vec::new();
  let mut err = Vec::new();
  let mut sinks = OutputSinks::new(step_id, events, stdout);
  while let Some(item) = output.next().await {
    let (buffer, bytes, stderr) = match item? {
      LogOutput::StdOut { message } | LogOutput::Console { message } => (&mut out, message, false),
      LogOutput::StdErr { message } => (&mut err, message, true),
      LogOutput::StdIn { .. } => continue,
    };
    buffer.extend_from_slice(&bytes);
    sinks.forward_complete_lines(buffer, stderr).await;
  }
  sinks.flush_remaining(&out, &err).await;
  Ok(())
}

struct OutputSinks<'a> {
  step_id: &'a str,
  events: &'a mpsc::Sender<RunnerEvent>,
  stdout: &'a mpsc::Sender<String>,
  stdout_open: bool,
  stderr_open: bool,
}

impl<'a> OutputSinks<'a> {
  fn new(
    step_id: &'a str,
    events: &'a mpsc::Sender<RunnerEvent>,
    stdout: &'a mpsc::Sender<String>,
  ) -> Self {
    Self {
      step_id,
      events,
      stdout,
      stdout_open: true,
      stderr_open: true,
    }
  }

  async fn flush_remaining(&mut self, out: &[u8], err: &[u8]) {
    if !out.is_empty() {
      self.forward_line(out, false).await;
    }
    if !err.is_empty() {
      self.forward_line(err, true).await;
    }
  }

  async fn forward_complete_lines(&mut self, buffer: &mut Vec<u8>, stderr: bool) {
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
      let mut line: Vec<_> = buffer.drain(..=end).collect();
      line.pop();
      if line.last() == Some(&b'\r') {
        line.pop();
      }
      self.forward_line(&line, stderr).await;
    }
  }

  async fn forward_line(&mut self, bytes: &[u8], stderr: bool) {
    let line = String::from_utf8_lossy(bytes).into_owned();
    if !stderr || line.starts_with("::") {
      if self.stdout_open && self.stdout.send(line).await.is_err() {
        self.stdout_open = false;
        tracing::warn!("Docker action command receiver dropped; continuing");
      }
      return;
    }
    if self.stderr_open
      && self
        .events
        .send(RunnerEvent::Log {
          step_id: self.step_id.to_owned(),
          line,
          stream: LogStream::Stderr,
        })
        .await
        .is_err()
    {
      self.stderr_open = false;
      tracing::warn!("Docker action stderr event receiver dropped; continuing");
    }
  }
}
