//! Docker API transport pinned before workflow environment is evaluated.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use shared::{RunnerError, SecretMasker};

/// One job's Docker connection and diagnostic redaction.
pub(crate) struct ContainerCommand {
  pub(crate) docker: Docker,
  masker: Arc<Mutex<SecretMasker>>,
}

impl ContainerCommand {
  /// Resolve a local socket, including a Docker CLI context when host is unset.
  pub(crate) async fn connect(masker: Arc<Mutex<SecretMasker>>) -> Result<Self, RunnerError> {
    let endpoint = if let Some(host) = crate::config::docker_host().filter(|v| !v.trim().is_empty())
    {
      host.trim().to_owned()
    } else {
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
        .map_err(|e| {
          RunnerError::Docker(format!(
            "resolve Docker context: {e}; set DOCKER_HOST or install the Docker CLI"
          ))
        })?;
      if !output.status.success() {
        return Err(RunnerError::Docker(
          "cannot resolve Docker context; set DOCKER_HOST to the local daemon socket".to_owned(),
        ));
      }
      String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    if !endpoint.starts_with("unix://") {
      return Err(RunnerError::Docker(
        "job containers require a local Linux Docker daemon using a unix socket".to_owned(),
      ));
    }
    let docker = Docker::connect_with_host(&endpoint)
      .map_err(|e| RunnerError::Docker(format!("connect local Docker daemon: {e}")))?;
    let client = Self { docker, masker };
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

  /// Mask daemon errors before they enter diagnostics or an event stream.
  pub(crate) fn error(&self, operation: &str, error: impl std::fmt::Display) -> RunnerError {
    let guard = self
      .masker
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    RunnerError::Docker(format!("{operation}: {}", guard.mask(&error.to_string())))
  }
}
