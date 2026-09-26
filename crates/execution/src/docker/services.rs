//! Owned service containers and explicit job-scoped teardown.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::models::{ContainerCreateBody, NetworkCreateRequest};
use bollard::query_parameters::{CreateContainerOptions, LogsOptions, RemoveContainerOptions};
use expressions::types::ExprValue;
use futures_util::StreamExt;
use shared::{LogStream, RunnerError, RunnerEvent, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::container_command::ContainerCommand;
use super::service_create::{create_body, owner_label, pull};
use super::service_health::wait_healthy;
use super::service_spec::ServiceSpec;

struct ServiceContainer {
  alias: String,
  name: String,
  id: String,
}

/// Services owned by one job, borrowing the job-container network when present.
pub(crate) struct ServiceContainers {
  transport: ContainerCommand,
  network: String,
  owns_network: bool,
  containers: Vec<ServiceContainer>,
  context: HashMap<String, ExprValue>,
}

impl ServiceContainers {
  /// Start all services and wait until ready, cleaning partial failures.
  pub(crate) async fn start(
    specs: &[ServiceSpec],
    network: Option<&str>,
    masker: Arc<Mutex<SecretMasker>>,
    cancel: &CancellationToken,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Result<Option<Self>, RunnerError> {
    if specs.is_empty() || cancel.is_cancelled() {
      return Ok(None);
    }
    register_credentials(specs, &masker);
    let transport = ContainerCommand::connect(masker).await?;
    let mut group = Self {
      transport,
      network: network.map_or_else(
        || format!("toolu_services_{}", uuid::Uuid::new_v4().simple()),
        ToOwned::to_owned,
      ),
      owns_network: network.is_none(),
      containers: Vec::new(),
      context: HashMap::new(),
    };
    if let Err(error) = group.initialize(specs, cancel, events).await {
      let cleanup = group.cleanup(events).await;
      if let Err(cleanup) = cleanup {
        return Err(RunnerError::Docker(format!(
          "{error}; service cleanup also failed: {cleanup}"
        )));
      }
      if cancel.is_cancelled() {
        return Ok(None);
      }
      return Err(error);
    }
    Ok(Some(group))
  }

  async fn initialize(
    &mut self,
    specs: &[ServiceSpec],
    cancel: &CancellationToken,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Result<(), RunnerError> {
    // Validate every create body before creating the first owned resource.
    let bodies = specs
      .iter()
      .map(|spec| create_body(spec, &self.network))
      .collect::<Result<Vec<_>, _>>()?;
    check_cancel(cancel)?;
    if self.owns_network {
      self
        .transport
        .docker
        .create_network(NetworkCreateRequest {
          name: self.network.clone(),
          labels: Some(owner_label()),
          ..Default::default()
        })
        .await
        .map_err(|e| self.transport.error("create service network", e))?;
    }
    for (spec, body) in specs.iter().zip(bodies) {
      self.start_one(spec, body, cancel, events).await?;
    }
    for container in &self.containers {
      check_cancel(cancel)?;
      wait_healthy(
        &self.transport,
        &container.id,
        &container.alias,
        cancel,
        events,
        Duration::from_secs(300),
      )
      .await?;
    }
    check_cancel(cancel)
  }

  async fn start_one(
    &mut self,
    spec: &ServiceSpec,
    body: ContainerCreateBody,
    cancel: &CancellationToken,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Result<(), RunnerError> {
    check_cancel(cancel)?;
    pull(&self.transport, spec, cancel).await?;
    check_cancel(cancel)?;
    let id = self.create_owned(spec, body).await?;
    service_log(
      events,
      format!("Service {} created: {} on {}", spec.alias, id, self.network),
    )
    .await;
    check_cancel(cancel)?;
    self
      .transport
      .docker
      .start_container(&id, None)
      .await
      .map_err(|error| self.transport.error("start service container", error))?;
    service_log(
      events,
      format!("Service {} started: {} on {}", spec.alias, id, self.network),
    )
    .await;
    self.inspect_context(&spec.alias, &id).await
  }

  async fn create_owned(
    &mut self,
    spec: &ServiceSpec,
    body: ContainerCreateBody,
  ) -> Result<String, RunnerError> {
    let name = format!(
      "toolu_service_{}_{}",
      spec.alias,
      uuid::Uuid::new_v4().simple()
    );
    self.containers.push(ServiceContainer {
      alias: spec.alias.clone(),
      name: name.clone(),
      id: String::new(),
    });
    let created = self
      .transport
      .docker
      .create_container(
        Some(CreateContainerOptions {
          name: Some(name),
          ..Default::default()
        }),
        body,
      )
      .await
      .map_err(|error| self.transport.error("create service container", error))?;
    let container = self
      .containers
      .last_mut()
      .ok_or_else(|| RunnerError::Docker("missing owned service".to_owned()))?;
    container.id.clone_from(&created.id);
    Ok(created.id)
  }
}

impl ServiceContainers {
  async fn inspect_context(&mut self, alias: &str, id: &str) -> Result<(), RunnerError> {
    let inspected = self
      .transport
      .docker
      .inspect_container(id, None)
      .await
      .map_err(|e| self.transport.error("inspect service ports", e))?;
    let mut ports = HashMap::new();
    for (port, bindings) in inspected
      .network_settings
      .and_then(|settings| settings.ports)
      .unwrap_or_default()
    {
      if let Some(host_port) = bindings
        .and_then(|bindings| bindings.into_iter().next())
        .and_then(|binding| binding.host_port)
      {
        let port = port.split('/').next().unwrap_or(&port).to_owned();
        ports.insert(port, ExprValue::String(host_port));
      }
    }
    self.context.insert(
      alias.to_owned(),
      ExprValue::object(HashMap::from([
        ("id".to_owned(), ExprValue::String(id.to_owned())),
        (
          "network".to_owned(),
          ExprValue::String(self.network.clone()),
        ),
        ("ports".to_owned(), ExprValue::object(ports)),
      ])),
    );
    Ok(())
  }

  /// Actual runtime IDs and published ports for expression evaluation.
  pub(crate) fn context(&self) -> HashMap<String, ExprValue> {
    self.context.clone()
  }
}

impl ServiceContainers {
  /// Dump logs and attempt every removal, even after a failure or cancellation.
  pub(crate) async fn cleanup(
    &self,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Result<(), RunnerError> {
    let mut errors = Vec::new();
    for container in self.containers.iter().rev() {
      if !container.id.is_empty()
        && let Err(error) = self.dump_logs(container, events).await
      {
        errors.push(error.to_string());
      }
      let removal = self
        .transport
        .docker
        .remove_container(
          &container.name,
          Some(RemoveContainerOptions {
            force: true,
            ..Default::default()
          }),
        )
        .await;
      if let Err(error) = removal
        && !is_missing(&error)
      {
        errors.push(
          self
            .transport
            .error("remove service container", error)
            .to_string(),
        );
      }
    }
    if self.owns_network
      && let Err(error) = self.transport.docker.remove_network(&self.network).await
      && !is_missing(&error)
    {
      errors.push(
        self
          .transport
          .error("remove service network", error)
          .to_string(),
      );
    }
    if errors.is_empty() {
      Ok(())
    } else {
      Err(RunnerError::Docker(errors.join("; ")))
    }
  }

  async fn dump_logs(
    &self,
    container: &ServiceContainer,
    events: &mpsc::Sender<RunnerEvent>,
  ) -> Result<(), RunnerError> {
    let mut stream = self.transport.docker.logs(
      &container.id,
      Some(LogsOptions {
        stdout: true,
        stderr: true,
        follow: false,
        ..Default::default()
      }),
    );
    tokio::time::timeout(Duration::from_secs(30), async {
      while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
          Ok(chunk) => chunk,
          Err(error) if is_missing(&error) => return Ok(()),
          Err(error) => return Err(self.transport.error("read service logs", error)),
        };
        for line in chunk.to_string().lines() {
          service_log(events, format!("[service {}] {line}", container.alias)).await;
        }
      }
      Ok(())
    })
    .await
    .map_err(|elapsed| {
      self
        .transport
        .error("service log collection timed out", elapsed)
    })?
  }
}

pub(super) async fn service_log(events: &mpsc::Sender<RunnerEvent>, line: String) {
  if events
    .send(RunnerEvent::Log {
      step_id: String::new(),
      line,
      stream: LogStream::Stdout,
    })
    .await
    .is_err()
  {
    tracing::warn!("event receiver closed while reporting service container logs");
  }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), RunnerError> {
  if cancel.is_cancelled() {
    Err(RunnerError::Docker("service setup cancelled".to_owned()))
  } else {
    Ok(())
  }
}

fn is_missing(error: &bollard::errors::Error) -> bool {
  matches!(
    error,
    bollard::errors::Error::DockerResponseServerError {
      status_code: 404,
      ..
    }
  )
}

fn register_credentials(specs: &[ServiceSpec], masker: &Arc<Mutex<SecretMasker>>) {
  let mut guard = match masker.lock() {
    Ok(guard) => guard,
    Err(poisoned) => poisoned.into_inner(),
  };
  for spec in specs {
    if let Some(auth) = &spec.container.credentials {
      guard.add_secrets([&auth.username, &auth.password]);
    }
  }
}
