use super::client::{CreateParams, DockerClient};
use shared::RunnerError;

/// A running service container.
#[derive(Debug)]
pub struct ServiceContainer {
  pub alias: String,
  pub container_id: String,
  pub image: String,
}

/// Start a service container on the job network.
///
/// # Errors
///
/// Returns `RunnerError::Docker` on start failures.
pub async fn start_service(
  client: &DockerClient,
  image: &str,
  alias: &str,
  network: &str,
  job_uuid: &str,
  env: &[String],
) -> Result<ServiceContainer, RunnerError> {
  let container_name = format!("github_svc_{alias}_{job_uuid}");

  client.pull_image(image, "latest", None).await?;

  let full_image = if image.contains(':') {
    image.to_owned()
  } else {
    format!("{image}:latest")
  };

  let params = CreateParams {
    image: &full_image,
    name: Some(&container_name),
    cmd: vec![],
    env,
    binds: &[],
    network: Some(network),
  };

  let container_id = client.create_container(&params).await?;
  client.start_container(&container_id).await?;

  tracing::info!(
    alias,
    container_id = container_id.as_str(),
    "service started"
  );

  Ok(ServiceContainer {
    alias: alias.to_owned(),
    container_id,
    image: full_image,
  })
}
