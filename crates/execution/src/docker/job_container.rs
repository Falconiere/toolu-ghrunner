//! Owned job container, shared network identity and explicit end-of-job cleanup.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bollard::auth::DockerCredentials;
use bollard::models::{ContainerCreateBody, HostConfig, NetworkCreateRequest};
use bollard::query_parameters::{
  CreateContainerOptions, CreateImageOptions, RemoveContainerOptions,
};
use futures_util::StreamExt;
use shared::{RunnerConfig, RunnerError, SecretMasker};
use tokio_util::sync::CancellationToken;

use super::container_command::ContainerCommand;
use super::container_create_options::apply_container_options;
use super::container_mounts::{ContainerMounts, validate_volume};
use super::container_ports::apply_ports;
use super::container_spec::{ContainerCredentials, ContainerSpec};
use super::path_translator::PathTranslator;

/// Job execution host. The caller must await cleanup after all main/post steps.
pub struct JobContainer {
  pub(crate) transport: ContainerCommand,
  name: String,
  id: String,
  network: String,
  mounts: ContainerMounts,
  image_path: String,
  /// Effective container CI value inherited by execs without a step override.
  pub(crate) base_ci: String,
}

impl JobContainer {
  /// Pull, create and start a container on a new per-job network.
  ///
  /// Returns `None` when setup is cancelled and owned resources are clean.
  /// Cancellation is checked between completed mutations so cleanup cannot race
  /// a dropped in-flight create. Setup errors clean all partially owned resources.
  /// This adapter supports a macOS client for real-daemon tests; the production
  /// job entrypoint enforces Linux-only execution.
  ///
  /// # Errors
  /// Returns a masked Docker diagnostic on invalid setup, cancellation or failure.
  pub async fn start(
    spec: &ContainerSpec,
    config: &RunnerConfig,
    workspace: &Path,
    masker: Arc<Mutex<SecretMasker>>,
    cancel: &CancellationToken,
  ) -> Result<Option<Self>, RunnerError> {
    if cancel.is_cancelled() {
      return Ok(None);
    }
    for volume in &spec.volumes {
      validate_volume(volume)?;
    }
    register_credentials(spec.credentials.as_ref(), &masker);
    let transport = ContainerCommand::connect(masker).await?;
    let mounts = materialize_mounts(config, workspace).await?;
    if cancel.is_cancelled() {
      return Ok(None);
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let base_ci = spec
      .env
      .get("CI")
      .cloned()
      .or_else(|| crate::config::var("CI"))
      .unwrap_or_else(|| "true".to_owned());
    let mut container = Self {
      transport,
      name: format!("toolu_job_{suffix}"),
      id: String::new(),
      network: format!("toolu_network_{suffix}"),
      mounts,
      image_path: String::new(),
      base_ci,
    };
    if let Err(error) = container.initialize(spec, cancel).await {
      if let Err(cleanup) = container.cleanup().await {
        return Err(RunnerError::Docker(format!(
          "{error}; cleanup also failed: {cleanup}"
        )));
      }
      if cancel.is_cancelled() {
        return Ok(None);
      }
      return Err(error);
    }
    Ok(Some(container))
  }

  async fn initialize(
    &mut self,
    spec: &ContainerSpec,
    cancel: &CancellationToken,
  ) -> Result<(), RunnerError> {
    let body = self.create_body(spec)?;
    check_cancel(cancel)?;
    self.pull(spec, cancel).await?;
    check_cancel(cancel)?;
    self.create_network().await?;
    check_cancel(cancel)?;
    let docker = &self.transport.docker;
    let created = docker
      .create_container(
        Some(CreateContainerOptions {
          name: Some(self.name.clone()),
          ..Default::default()
        }),
        body,
      )
      .await
      .map_err(|e| self.transport.error("create job container", e))?;
    self.id = created.id;
    check_cancel(cancel)?;
    docker
      .start_container(&self.id, None)
      .await
      .map_err(|e| self.transport.error("start job container", e))?;
    check_cancel(cancel)?;
    let inspected = docker
      .inspect_container(&self.id, None)
      .await
      .map_err(|e| self.transport.error("inspect job container", e))?;
    if !inspected.state.and_then(|s| s.running).unwrap_or(false) {
      return Err(RunnerError::Docker("job container exited during startup; image must provide tail and the requested shell/runtime".to_owned()));
    }
    inspected
      .config
      .and_then(|c| c.env)
      .unwrap_or_default()
      .iter()
      .find_map(|entry| entry.strip_prefix("PATH="))
      .unwrap_or_default()
      .clone_into(&mut self.image_path);
    check_cancel(cancel)
  }

  async fn create_network(&self) -> Result<(), RunnerError> {
    self
      .transport
      .docker
      .create_network(NetworkCreateRequest {
        name: self.network.clone(),
        labels: Some(owner_label()),
        ..Default::default()
      })
      .await
      .map_err(|e| self.transport.error("create job network", e))?;
    Ok(())
  }

  fn create_body(&self, spec: &ContainerSpec) -> Result<ContainerCreateBody, RunnerError> {
    let mut env = spec.env.clone();
    env.insert("HOME".to_owned(), "/github/home".to_owned());
    crate::execution::step_process_env::apply(&mut env, Some(&self.base_ci));
    let mut body = ContainerCreateBody {
      image: Some(spec.image.clone()),
      entrypoint: Some(vec!["tail".to_owned()]),
      cmd: Some(vec!["-f".to_owned(), "/dev/null".to_owned()]),
      working_dir: Some("/github/workspace".to_owned()),
      env: Some(
        env
          .into_iter()
          .map(|(key, value)| format!("{key}={value}"))
          .collect(),
      ),
      labels: Some(owner_label()),
      host_config: Some(HostConfig {
        binds: Some(
          self
            .mounts
            .volumes
            .iter()
            .chain(&spec.volumes)
            .cloned()
            .collect(),
        ),
        network_mode: Some(self.network.clone()),
        ..Default::default()
      }),
      ..Default::default()
    };
    apply_container_options(&spec.options, &mut body)?;
    apply_ports(&spec.ports, &mut body)?;
    Ok(body)
  }
}

impl JobContainer {
  async fn pull(
    &self,
    spec: &ContainerSpec,
    cancel: &CancellationToken,
  ) -> Result<(), RunnerError> {
    let credentials = spec.credentials.as_ref().map(|auth| DockerCredentials {
      username: Some(auth.username.clone()),
      password: Some(auth.password.clone()),
      ..Default::default()
    });
    let options = CreateImageOptions {
      from_image: Some(spec.image.clone()),
      ..Default::default()
    };
    let mut stream = self
      .transport
      .docker
      .create_image(Some(options), None, credentials);
    loop {
      let next = tokio::select! {
        () = cancel.cancelled() => return check_cancel(cancel),
        next = stream.next() => next,
      };
      let Some(next) = next else {
        return Ok(());
      };
      let info = next.map_err(|e| self.transport.error("pull job image", e))?;
      if let Some(error) = info.error_detail.and_then(|detail| detail.message) {
        return Err(self.transport.error("pull job image", error));
      }
    }
  }

  /// Docker container id exposed as `job.container.id`.
  pub fn id(&self) -> &str {
    &self.id
  }

  /// Network shared by the job container and future service/action containers.
  pub fn network(&self) -> &str {
    &self.network
  }

  /// PATH inherited from the image and container declaration.
  pub fn image_path(&self) -> &str {
    &self.image_path
  }

  /// Host temp directory mounted into the container for step scripts.
  pub fn temp_dir(&self) -> &Path {
    &self.mounts.temp
  }

  /// Explicit mount mappings used by both expressions and process arguments.
  pub fn translator(&self) -> &PathTranslator {
    &self.mounts.translator
  }

  /// Translate path-valued environment entries, including PATH lists.
  pub fn translate_value(&self, value: &str) -> String {
    value
      .split(':')
      .map(|part| {
        self
          .translator()
          .to_container(Path::new(part))
          .to_string_lossy()
          .into_owned()
      })
      .collect::<Vec<_>>()
      .join(":")
  }

  /// Translate runner-owned path variables while preserving opaque workflow data.
  pub fn translate_env(&self, key: &str, value: &str) -> String {
    if key == "PATH" {
      return self.translate_value(value);
    }
    if matches!(
      key,
      "GITHUB_WORKSPACE"
        | "GITHUB_ACTION_PATH"
        | "GITHUB_EVENT_PATH"
        | "GITHUB_ENV"
        | "GITHUB_OUTPUT"
        | "GITHUB_PATH"
        | "GITHUB_STATE"
        | "GITHUB_STEP_SUMMARY"
        | "RUNNER_TEMP"
        | "RUNNER_TOOL_CACHE"
    ) {
      return self
        .translator()
        .to_container(Path::new(value))
        .to_string_lossy()
        .into_owned();
    }
    value.to_owned()
  }

  /// Force-remove the owned container, then independently remove its network.
  /// Missing resources are already clean; user-supplied volumes are never removed.
  ///
  /// # Errors
  /// Returns both cleanup errors when Docker cannot remove owned resources.
  pub async fn cleanup(&self) -> Result<(), RunnerError> {
    let container = self
      .transport
      .docker
      .remove_container(
        &self.name,
        Some(RemoveContainerOptions {
          force: true,
          ..Default::default()
        }),
      )
      .await;
    let network = self.transport.docker.remove_network(&self.network).await;
    let errors: Vec<_> = [container, network]
      .into_iter()
      .filter_map(Result::err)
      .filter(|error| {
        !matches!(
          error,
          bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
          }
        )
      })
      .map(|error| {
        self
          .transport
          .error("remove job resources", error)
          .to_string()
      })
      .collect();
    if errors.is_empty() {
      Ok(())
    } else {
      Err(RunnerError::Docker(errors.join("; ")))
    }
  }
}

async fn materialize_mounts(
  config: &RunnerConfig,
  workspace: &Path,
) -> Result<ContainerMounts, RunnerError> {
  let mount_config = config.clone();
  let mount_workspace = workspace.to_path_buf();
  tokio::task::spawn_blocking(move || ContainerMounts::create(&mount_config, &mount_workspace))
    .await
    .map_err(|error| RunnerError::Docker(format!("materialize job container mounts: {error}")))?
}

fn register_credentials(
  credentials: Option<&ContainerCredentials>,
  masker: &Arc<Mutex<SecretMasker>>,
) {
  let Some(credentials) = credentials else {
    return;
  };
  let (mut guard, recovered) = match masker.lock() {
    Ok(guard) => (guard, false),
    Err(poisoned) => (poisoned.into_inner(), true),
  };
  guard.add_secrets([&credentials.username, &credentials.password]);
  drop(guard);
  if recovered {
    tracing::warn!("recovered poisoned Docker secret masker mutex");
  }
}

fn owner_label() -> HashMap<String, String> {
  HashMap::from([("io.toolu.job-container".to_owned(), "true".to_owned())])
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), RunnerError> {
  if cancel.is_cancelled() {
    Err(RunnerError::Docker(
      "job container setup cancelled".to_owned(),
    ))
  } else {
    Ok(())
  }
}

#[cfg(test)]
#[path = "tests/job_container.rs"]
mod tests;
