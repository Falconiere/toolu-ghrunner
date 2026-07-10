use bollard::Docker;
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
  CreateContainerOptions, CreateImageOptions, KillContainerOptions, RemoveContainerOptions,
  StartContainerOptions, WaitContainerOptions,
};
use futures_util::StreamExt;

use shared::RunnerError;

/// Registry authentication for pulling private images.
#[derive(Debug, Clone)]
pub struct RegistryAuth {
  pub username: String,
  pub password: String,
  pub server_address: Option<String>,
}

/// Inspected image info.
#[derive(Debug)]
pub struct ImageInfo {
  pub repo_tags: Vec<String>,
}

/// Parameters for creating a container.
pub struct CreateParams<'a> {
  pub image: &'a str,
  pub name: Option<&'a str>,
  pub cmd: Vec<&'a str>,
  pub env: &'a [String],
  pub binds: &'a [String],
  pub network: Option<&'a str>,
}

/// Thin wrapper over bollard providing core Docker operations.
#[derive(Clone)]
pub struct DockerClient {
  inner: Docker,
}

impl DockerClient {
  /// Connect to the local Docker daemon.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` if the daemon is not available.
  pub fn new() -> Result<Self, RunnerError> {
    let docker = Docker::connect_with_socket_defaults()
      .map_err(|e| RunnerError::Docker(format!("connect docker daemon: {e}")))?;
    Ok(Self { inner: docker })
  }

  /// Access the underlying bollard client.
  pub fn inner(&self) -> &Docker {
    &self.inner
  }

  /// Pull a Docker image from a registry.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` on pull failures.
  pub async fn pull_image(
    &self,
    image: &str,
    tag: &str,
    _auth: Option<&RegistryAuth>,
  ) -> Result<(), RunnerError> {
    let options = CreateImageOptions {
      from_image: Some(image.to_owned()),
      tag: Some(tag.to_owned()),
      ..Default::default()
    };

    let mut stream = self.inner.create_image(Some(options), None, None);
    while let Some(result) = stream.next().await {
      result.map_err(|e| RunnerError::Docker(format!("pull image: {e}")))?;
    }
    Ok(())
  }

  /// Inspect an image.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` if the image doesn't exist.
  pub async fn inspect_image(&self, image: &str) -> Result<ImageInfo, RunnerError> {
    let info = self
      .inner
      .inspect_image(image)
      .await
      .map_err(|e| RunnerError::Docker(format!("inspect image: {e}")))?;
    Ok(ImageInfo {
      repo_tags: info.repo_tags.unwrap_or_default(),
    })
  }

  /// Create a container with the given configuration.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` on creation failures.
  pub async fn create_container(&self, params: &CreateParams<'_>) -> Result<String, RunnerError> {
    let options = params.name.map(|n| CreateContainerOptions {
      name: Some(n.to_owned()),
      ..Default::default()
    });

    let host_config = bollard::models::HostConfig {
      binds: Some(params.binds.to_vec()),
      network_mode: params.network.map(ToOwned::to_owned),
      ..Default::default()
    };

    let config = ContainerCreateBody {
      image: Some(params.image.to_owned()),
      cmd: Some(params.cmd.iter().map(|s| (*s).to_owned()).collect()),
      env: Some(params.env.to_vec()),
      host_config: Some(host_config),
      working_dir: Some("/github/workspace".to_owned()),
      ..Default::default()
    };

    let response = self
      .inner
      .create_container(options, config)
      .await
      .map_err(|e| RunnerError::Docker(format!("create container: {e}")))?;
    Ok(response.id)
  }

  /// Start a container.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` on start failures.
  pub async fn start_container(&self, container_id: &str) -> Result<(), RunnerError> {
    self
      .inner
      .start_container(container_id, None::<StartContainerOptions>)
      .await
      .map_err(|e| RunnerError::Docker(format!("start container: {e}")))?;
    Ok(())
  }

  /// Wait for a container to exit and return its exit code.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` on wait failures.
  pub async fn wait_container(&self, container_id: &str) -> Result<i64, RunnerError> {
    let options = WaitContainerOptions {
      condition: "not-running".to_owned(),
    };
    let mut stream = self.inner.wait_container(container_id, Some(options));
    if let Some(result) = stream.next().await {
      let response = result.map_err(|e| RunnerError::Docker(format!("wait container: {e}")))?;
      return Ok(response.status_code);
    }
    Ok(-1)
  }

  /// Remove a container (force).
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` on removal failures.
  pub async fn remove_container(&self, container_id: &str) -> Result<(), RunnerError> {
    let options = RemoveContainerOptions {
      force: true,
      ..Default::default()
    };
    self
      .inner
      .remove_container(container_id, Some(options))
      .await
      .map_err(|e| RunnerError::Docker(format!("remove container: {e}")))?;
    Ok(())
  }

  /// Kill a container with SIGKILL.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Docker` on kill failures.
  pub async fn kill_container(&self, container_id: &str) -> Result<(), RunnerError> {
    self
      .inner
      .kill_container(container_id, None::<KillContainerOptions>)
      .await
      .map_err(|e| RunnerError::Docker(format!("kill container: {e}")))?;
    Ok(())
  }
}

impl std::fmt::Debug for DockerClient {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("DockerClient").finish()
  }
}
