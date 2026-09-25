//! A real daemon stuck in starting state must respect the readiness budget.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HealthConfig};
use bollard::query_parameters::{CreateContainerOptions, RemoveContainerOptions};
use shared::SecretMasker;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::container_command::ContainerCommand;
use super::wait_healthy;

#[tokio::test]
#[ignore = "requires a real Linux Docker daemon and locally pulled nginx:1.27-alpine"]
async fn never_healthy_service_respects_budget() -> Result<(), Box<dyn std::error::Error>> {
  let transport = ContainerCommand::connect(Arc::new(Mutex::new(SecretMasker::new()))).await?;
  let name = format!("toolu_health_test_{}", uuid::Uuid::new_v4().simple());
  let created = transport
    .docker
    .create_container(
      Some(CreateContainerOptions {
        name: Some(name),
        ..Default::default()
      }),
      ContainerCreateBody {
        image: Some("nginx:1.27-alpine".to_owned()),
        healthcheck: Some(HealthConfig {
          test: Some(vec!["CMD-SHELL".to_owned(), "false".to_owned()]),
          start_period: Some(3_600_000_000_000),
          interval: Some(1_000_000_000),
          ..Default::default()
        }),
        ..Default::default()
      },
    )
    .await?;
  let result = async {
    transport.docker.start_container(&created.id, None).await?;
    let (events, mut receiver) = mpsc::channel(16);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let started = tokio::time::Instant::now();
    let result = wait_healthy(
      &transport,
      &created.id,
      "never",
      &CancellationToken::new(),
      &events,
      Duration::from_millis(150),
    )
    .await;
    drop(events);
    drain.await?;
    assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok::<(), Box<dyn std::error::Error>>(())
  }
  .await;
  transport
    .docker
    .remove_container(
      &created.id,
      Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
      }),
    )
    .await?;
  result
}
