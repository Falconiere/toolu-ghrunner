//! Docker API transport pinned before workflow environment is evaluated.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use shared::{RunnerError, SecretMasker};

/// One job's Docker connection and diagnostic redaction.
pub(crate) struct ContainerCommand {
  pub(crate) docker: Docker,
  socket_path: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
}

impl ContainerCommand {
  /// Resolve a local socket, including a Docker CLI context when host is unset.
  pub(crate) async fn connect(masker: Arc<Mutex<SecretMasker>>) -> Result<Self, RunnerError> {
    let endpoint = resolve_endpoint().await?;
    if !endpoint.starts_with("unix://") {
      return Err(RunnerError::Docker(
        "job containers require a local Linux Docker daemon using a unix socket".to_owned(),
      ));
    }
    let socket_path = endpoint
      .strip_prefix("unix://")
      .map(PathBuf::from)
      .filter(|path| path.is_absolute())
      .ok_or_else(|| {
        RunnerError::Docker("Docker daemon socket must be an absolute unix path".to_owned())
      })?;
    let docker = Docker::connect_with_host(&endpoint)
      .map_err(|e| RunnerError::Docker(format!("connect local Docker daemon: {e}")))?;
    let client = Self {
      docker,
      socket_path,
      masker,
    };
    let info = client
      .docker
      .version()
      .await
      .map_err(|e| client.error("inspect Docker server", e))?;
    if info.os.as_deref() != Some("linux") {
      return Err(RunnerError::Docker(
        "job containers require a Linux Docker daemon".to_owned(),
      ));
    }
    Ok(client)
  }

  /// Return the socket selected for this exact Docker transport.
  pub(crate) fn socket_path(&self) -> &Path {
    &self.socket_path
  }

  /// Mask daemon errors before they enter diagnostics or an event stream.
  pub(crate) fn error(&self, operation: &str, error: impl std::fmt::Display) -> RunnerError {
    let (guard, recovered) = match self.masker.lock() {
      Ok(guard) => (guard, false),
      Err(poisoned) => (poisoned.into_inner(), true),
    };
    let masked_error = guard.mask(&error.to_string()).into_owned();
    drop(guard);
    if recovered {
      tracing::warn!("recovered poisoned Docker secret masker mutex");
    }
    RunnerError::Docker(format!("{operation}: {masked_error}"))
  }
}

async fn resolve_endpoint() -> Result<String, RunnerError> {
  if let Some(host) = crate::config::docker_host().filter(|value| !value.trim().is_empty()) {
    return Ok(host.trim().to_owned());
  }
  let mut command = tokio::process::Command::new("docker");
  command.args([
    "context",
    "inspect",
    "--format",
    "{{.Endpoints.docker.Host}}",
  ]);
  command.kill_on_drop(true);
  let output = tokio::time::timeout(Duration::from_secs(30), command.output())
    .await
    .map_err(|_elapsed| RunnerError::Docker("Docker context lookup timed out".to_owned()))?
    .map_err(|error| {
      RunnerError::Docker(format!(
        "resolve Docker context: {error}; set DOCKER_HOST or install the Docker CLI"
      ))
    })?;
  if !output.status.success() {
    return Err(RunnerError::Docker(
      "cannot resolve Docker context; set DOCKER_HOST to the local daemon socket".to_owned(),
    ));
  }
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
