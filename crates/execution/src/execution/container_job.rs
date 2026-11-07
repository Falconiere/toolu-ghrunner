//! Job-container validation and ownership around the full main/post lifecycle.

use std::path::Path;
use std::sync::Arc;

use shared::{
  AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent, ServicesMode,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_support::emit_log;
use super::context::ExecutionContext;
use crate::docker::container_spec::ContainerSpec;
use crate::docker::job_container::JobContainer;
use crate::docker::service_spec::ServiceSpec;
use crate::docker::services::ServiceContainers;

/// Evaluate the declaration and reject unsupported hosts before workspace setup.
pub(super) fn evaluate_container(
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  ctx: &ExecutionContext,
) -> Result<Option<ContainerSpec>, RunnerError> {
  let spec = ContainerSpec::evaluate(msg.job_container.as_ref(), ctx)?;
  if spec.is_some() {
    if !cfg!(target_os = "linux") {
      return Err(RunnerError::Config(
        "job containers require a Linux runner host".to_owned(),
      ));
    }
    if config.services_mode != ServicesMode::Forwarder {
      return Err(RunnerError::Config("job containers require forwarder services mode; local cache service URLs are not reachable from the container".to_owned()));
    }
  }
  Ok(spec)
}

/// Attach one container for main/post steps; return false on clean cancellation.
pub(super) async fn start_container(
  specs: (Option<&ContainerSpec>, &[ServiceSpec]),
  config: &RunnerConfig,
  workspace: &Path,
  ctx: &mut ExecutionContext,
  cancel: &CancellationToken,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<bool, RunnerError> {
  if let Some(spec) = specs.0 {
    let Some(container) =
      JobContainer::start(spec, config, workspace, Arc::clone(ctx.masker()), cancel).await?
    else {
      return Ok(false);
    };
    ctx.set_job_container(Arc::new(container));
  }
  if !specs.1.is_empty() {
    let network = ctx
      .job_container()
      .map(|container| container.network().to_owned());
    let result = ServiceContainers::start(
      specs.1,
      network.as_deref(),
      Arc::clone(ctx.masker()),
      cancel,
      events,
    )
    .await;
    match result {
      Ok(Some(services)) => ctx.set_services(services),
      Ok(None) => {
        if let Some(container) = ctx.job_container() {
          container.cleanup().await?;
        }
        return Ok(false);
      },
      Err(error) => {
        return finish_container::<()>(ctx, Err(error), events)
          .await
          .map(|()| false);
      },
    }
  }
  Ok(true)
}

/// Upstream "Initialize containers" (`JobExtension`, condition `success()`): a
/// numbered pre-job row after the Docker Pull/Build rows. `None` when the job
/// declares neither a container nor services; otherwise the row's conclusion.
pub(super) async fn initialize_containers(
  specs: (Option<&ContainerSpec>, &[ServiceSpec]),
  config: &RunnerConfig,
  workspace: &Path,
  ctx: &mut ExecutionContext,
  cancel: &CancellationToken,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<Option<Conclusion>, RunnerError> {
  if specs.0.is_none() && specs.1.is_empty() {
    return Ok(None);
  }
  let number = 2u32
    .checked_add(ctx.docker_setup_count)
    .ok_or_else(|| RunnerError::StepExecution("container setup step number overflow".to_owned()))?;
  ctx.docker_setup_count = ctx.docker_setup_count.saturating_add(1);
  // Upstream registers "Stop containers" before it starts anything.
  ctx.stop_containers_pending = true;
  let id = uuid::Uuid::new_v4().to_string();
  start_row(events, &id, "Initialize containers", number).await;
  let conclusion = match start_container(specs, config, workspace, ctx, cancel, events).await {
    Ok(true) => Conclusion::Success,
    Ok(false) => Conclusion::Cancelled,
    Err(error) => {
      emit_log(events, &id, &format!("##[error]{error}")).await;
      Conclusion::Failure
    },
  };
  complete_row(events, id, conclusion).await;
  Ok(Some(conclusion))
}

/// Upstream "Stop containers" (a post-job step registered first, condition
/// `always()`): the last numbered row before "Complete job". `None` when no
/// "Initialize containers" ran; `next` numbers the row when no post row did.
pub(super) async fn stop_containers(
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  next: u32,
) -> Option<Conclusion> {
  if !std::mem::take(&mut ctx.stop_containers_pending) {
    return None;
  }
  let number = ctx.next_step_number.unwrap_or(next);
  ctx.next_step_number = Some(number.saturating_add(1));
  let id = uuid::Uuid::new_v4().to_string();
  start_row(events, &id, "Stop containers", number).await;
  let (container, services) = ctx.take_containers();
  let conclusion = match cleanup(services.as_deref(), container.as_deref(), events).await {
    Ok(()) => Conclusion::Success,
    Err(error) => {
      emit_log(events, &id, &format!("##[error]{error}")).await;
      Conclusion::Failure
    },
  };
  complete_row(events, id, conclusion).await;
  Some(conclusion)
}

async fn start_row(events: &mpsc::Sender<RunnerEvent>, id: &str, name: &str, number: u32) {
  send(
    events,
    RunnerEvent::StepStarted {
      step_id: id.to_owned(),
      step_name: name.to_owned(),
      step_number: number,
    },
  )
  .await;
  // Upstream: StepTelemetry.Action = display name, lower-cased, spaces to `_`.
  send(
    events,
    RunnerEvent::StepMetadata {
      step_id: id.to_owned(),
      kind: "runner".to_owned(),
      action: Some(name.to_ascii_lowercase().replace(' ', "_")),
      git_ref: None,
    },
  )
  .await;
}

async fn complete_row(events: &mpsc::Sender<RunnerEvent>, id: String, conclusion: Conclusion) {
  send(
    events,
    RunnerEvent::StepCompleted {
      step_id: id,
      conclusion,
      outputs: std::collections::HashMap::new(),
    },
  )
  .await;
}

async fn send(events: &mpsc::Sender<RunnerEvent>, event: RunnerEvent) {
  if events.send(event).await.is_err() {
    tracing::warn!("container step event receiver closed");
  }
}

/// Remove owned resources before reporting completion, preserving primary errors.
pub(super) async fn finish_container<T>(
  ctx: &ExecutionContext,
  body: Result<T, RunnerError>,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<T, RunnerError> {
  let cleanup = cleanup(
    ctx.services(),
    ctx.job_container().map(AsRef::as_ref),
    events,
  )
  .await;
  match (body, cleanup) {
    (Ok(result), Ok(())) => Ok(result),
    (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
    (Err(error), Err(cleanup)) => Err(RunnerError::Docker(format!(
      "{error}; container cleanup also failed: {cleanup}"
    ))),
  }
}

/// Remove the services, then the job container, keeping both errors.
async fn cleanup(
  services: Option<&ServiceContainers>,
  container: Option<&JobContainer>,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<(), RunnerError> {
  let services_cleanup = match services {
    Some(services) => services.cleanup(events).await,
    None => Ok(()),
  };
  let container_cleanup = match container {
    Some(container) => container.cleanup().await,
    None => Ok(()),
  };
  match (services_cleanup, container_cleanup) {
    (Ok(()), result) | (result, Ok(())) => result,
    (Err(service), Err(container)) => Err(RunnerError::Docker(format!("{service}; {container}"))),
  }
}
