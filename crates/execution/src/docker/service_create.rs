//! Docker service request construction and authenticated image pulls.

use std::collections::HashMap;

use bollard::auth::DockerCredentials;
use bollard::models::{ContainerCreateBody, EndpointSettings, HostConfig, NetworkingConfig};
use bollard::query_parameters::CreateImageOptions;
use futures_util::StreamExt;
use shared::RunnerError;
use tokio_util::sync::CancellationToken;

use super::container_command::ContainerCommand;
use super::container_create_options::apply_container_options;
use super::container_mounts::validate_volume;
use super::container_ports::apply_ports;
use super::service_spec::ServiceSpec;

pub(super) fn create_body(
  spec: &ServiceSpec,
  network: &str,
) -> Result<ContainerCreateBody, RunnerError> {
  for volume in &spec.container.volumes {
    validate_volume(volume)?;
  }
  let mut body = ContainerCreateBody {
    image: Some(spec.container.image.clone()),
    env: Some(
      spec
        .container
        .env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect(),
    ),
    labels: Some(owner_label()),
    host_config: Some(HostConfig {
      binds: Some(spec.container.volumes.clone()),
      network_mode: Some(network.to_owned()),
      ..Default::default()
    }),
    networking_config: Some(NetworkingConfig {
      endpoints_config: Some(HashMap::from([(
        network.to_owned(),
        EndpointSettings {
          aliases: Some(vec![spec.alias.clone()]),
          ..Default::default()
        },
      )])),
    }),
    ..Default::default()
  };
  apply_container_options(&spec.container.options, &mut body)?;
  apply_ports(&spec.container.ports, &mut body)?;
  Ok(body)
}

pub(super) fn owner_label() -> HashMap<String, String> {
  HashMap::from([("io.toolu.service-container".to_owned(), "true".to_owned())])
}

pub(super) async fn pull(
  transport: &ContainerCommand,
  spec: &ServiceSpec,
  cancel: &CancellationToken,
) -> Result<(), RunnerError> {
  let credentials = spec
    .container
    .credentials
    .as_ref()
    .map(|auth| DockerCredentials {
      username: Some(auth.username.clone()),
      password: Some(auth.password.clone()),
      ..Default::default()
    });
  let mut stream = transport.docker.create_image(
    Some(CreateImageOptions {
      from_image: Some(spec.container.image.clone()),
      ..Default::default()
    }),
    None,
    credentials,
  );
  loop {
    let next = tokio::select! {
      () = cancel.cancelled() => return Err(RunnerError::Docker("service setup cancelled".to_owned())),
      next = stream.next() => next,
    };
    let Some(next) = next else {
      return Ok(());
    };
    let info = next.map_err(|e| transport.error("pull service image", e))?;
    if let Some(error) = info.error_detail.and_then(|detail| detail.message) {
      return Err(transport.error("pull service image", error));
    }
  }
}
