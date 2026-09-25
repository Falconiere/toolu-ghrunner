//! Bounded, cancellation-aware Docker health readiness.

use std::time::Duration;

use bollard::models::HealthStatusEnum;
use shared::{RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::container_command::ContainerCommand;
use super::services::service_log;

pub(super) async fn wait_healthy(
  transport: &ContainerCommand,
  id: &str,
  alias: &str,
  cancel: &CancellationToken,
  events: &mpsc::Sender<RunnerEvent>,
  budget: Duration,
) -> Result<(), RunnerError> {
  let deadline = Instant::now() + budget;
  let mut delay = Duration::from_secs(2);
  loop {
    let inspect = tokio::select! {
      () = cancel.cancelled() => return Err(failure(alias, "setup cancelled")),
      result = tokio::time::timeout_at(deadline, transport.docker.inspect_container(id, None)) => {
        result.map_err(|elapsed| failure(alias, &format!("health check timed out: {elapsed}")))?
          .map_err(|e| transport.error("inspect service health", e))?
      },
    };
    let health = inspect
      .state
      .and_then(|state| state.health)
      .and_then(|health| health.status);
    match health {
      None | Some(HealthStatusEnum::NONE | HealthStatusEnum::EMPTY | HealthStatusEnum::HEALTHY) => {
        return Ok(());
      },
      Some(HealthStatusEnum::UNHEALTHY) => return Err(failure(alias, "reported unhealthy")),
      Some(HealthStatusEnum::STARTING) => {},
    }
    service_log(
      events,
      format!("Service {alias} is starting; waiting for its health check"),
    )
    .await;
    tokio::select! {
      () = cancel.cancelled() => return Err(failure(alias, "setup cancelled")),
      () = tokio::time::sleep_until(deadline) => return Err(failure(alias, "health check timed out")),
      () = tokio::time::sleep(delay) => {},
    }
    delay = (delay * 2).min(Duration::from_secs(32));
  }
}

fn failure(alias: &str, reason: &str) -> RunnerError {
  RunnerError::Docker(format!("service {alias}: {reason}"))
}

#[cfg(test)]
#[path = "tests/service_health.rs"]
mod tests;
