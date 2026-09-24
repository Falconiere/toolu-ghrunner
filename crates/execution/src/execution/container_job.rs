//! Job-container validation and ownership around the full main/post lifecycle.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, ServicesMode};
use tokio_util::sync::CancellationToken;

use super::context::ExecutionContext;
use crate::docker::container_spec::ContainerSpec;
use crate::docker::job_container::JobContainer;

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
  spec: Option<&ContainerSpec>,
  config: &RunnerConfig,
  workspace: &Path,
  ctx: &mut ExecutionContext,
  cancel: &CancellationToken,
) -> Result<bool, RunnerError> {
  if let Some(spec) = spec {
    let Some(container) =
      JobContainer::start(spec, config, workspace, Arc::clone(ctx.masker()), cancel).await?
    else {
      return Ok(false);
    };
    ctx.set_job_container(Arc::new(container));
  }
  Ok(true)
}

/// Remove owned resources before reporting completion, preserving primary errors.
pub(super) async fn finish_container(
  ctx: &ExecutionContext,
  body: Result<(Conclusion, HashMap<String, String>), RunnerError>,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let cleanup = match ctx.job_container() {
    Some(container) => container.cleanup().await,
    None => Ok(()),
  };
  match (body, cleanup) {
    (Ok(result), Ok(())) => Ok(result),
    (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
    (Err(error), Err(cleanup)) => Err(RunnerError::Docker(format!(
      "{error}; container cleanup also failed: {cleanup}"
    ))),
  }
}
