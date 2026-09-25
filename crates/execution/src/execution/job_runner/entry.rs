//! Job setup, execution and resource teardown with separate cancellation sources.

use shared::{
  AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent, SecretMasker,
};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
  ContainerStart, ContainerStartParams, JobOutcome, build_job_context_async, finish_job,
  prepare_job_workspace, prepared, start_job_container, start_job_services, stop_local_services,
};
use crate::execution::container_job::{evaluate_container, finish_container};
use crate::execution::job_cancellation::JobCancellation;
use crate::execution::job_teardown::JobTeardown;

/// Initialize, execute, and tear down one acquired job.
pub(super) async fn run(
  msg: AgentJobRequestMessage,
  config: &RunnerConfig,
  cancel: CancellationToken,
  events: mpsc::Sender<RunnerEvent>,
  masker: Arc<Mutex<SecretMasker>>,
  shutdown: CancellationToken,
) -> Result<JobTeardown, RunnerError> {
  // Shutdown also interrupts setup/hooks, without cancelling the caller's token.
  let cancel = cancel.child_token();
  let workspace = config.workspace_root.join(&msg.job_id);
  let (msg, mut ctx) = build_job_context_async(msg, config, masker, workspace.clone()).await?;
  ctx.cancellation = Some(JobCancellation::new(cancel.clone(), shutdown));
  let container_spec = evaluate_container(&msg, config, &ctx)?;
  let (workspace, workspace_gc) = prepare_job_workspace(config, &msg.job_id, workspace).await?;
  let (http, local) = start_job_services(config, &msg, &mut ctx).await?;
  let (local, workspace_gc) = match start_job_container(ContainerStartParams {
    spec: container_spec.as_ref(),
    config,
    workspace: &workspace,
    ctx: &mut ctx,
    cancel: &cancel,
    local,
    events: &events,
    job_id: &msg.job_id,
    workspace_gc,
  })
  .await?
  {
    ContainerStart::Continue(local, workspace_gc) => (local, workspace_gc),
    ContainerStart::Finished(teardown) => return Ok(teardown),
  };
  let inputs = prepared::Inputs {
    msg: &msg,
    config,
    cancel: &cancel,
    events: &events,
    workspace: &workspace,
    http: &http,
  };
  let body_result = prepared::execute(inputs, &mut ctx).await;
  let (conclusion, outputs) = match finish_container(&ctx, body_result).await {
    Ok(result) => result,
    Err(error) => {
      stop_local_services(local).await;
      return Err(error);
    },
  };
  let conclusion = if ctx
    .cancellation
    .as_ref()
    .is_some_and(|signal| signal.shutdown.is_cancelled())
  {
    Conclusion::Failure
  } else {
    conclusion
  };
  let outcome = JobOutcome {
    job_id: msg.job_id,
    conclusion,
    outputs,
  };
  Ok(finish_job(local, &events, outcome, workspace_gc).await)
}
