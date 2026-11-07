//! Runs an initialized job with one action fetcher and its acquired defaults.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
  JobBody, JobResult, build_shadow_observer, emit_job_started, run_completed_hook_best_effort,
  run_job_body,
};
use crate::docker::container_spec::ContainerSpec;
use crate::docker::service_spec::ServiceSpec;
use crate::execution::action_support::emit_log;
use crate::execution::actions::prefetch::{ActionFetcher, spawn_prefetch};
use crate::execution::container_job::initialize_containers;
use crate::execution::context::ExecutionContext;
use crate::execution::docker_prepare;
use crate::execution::job_hooks::{JobHookStage, run_job_hook};
use crate::execution::job_spec::JobSpec;

/// Inputs already prepared by the job entry point.
pub(super) struct Inputs<'a> {
  /// Acquired job message.
  pub(super) msg: &'a AgentJobRequestMessage,
  /// Runner configuration.
  pub(super) config: &'a RunnerConfig,
  /// Job cancellation token.
  pub(super) cancel: &'a CancellationToken,
  /// Job event sender.
  pub(super) events: &'a mpsc::Sender<RunnerEvent>,
  /// Created job workspace.
  pub(super) workspace: &'a Path,
  /// Reused job HTTP client.
  pub(super) http: &'a reqwest::Client,
  /// Evaluated job container and service declarations, started after the
  /// Docker Pull/Build rows as upstream's "Initialize containers".
  pub(super) containers: (Option<&'a ContainerSpec>, &'a [ServiceSpec]),
}

pub(super) async fn execute(
  inputs: Inputs<'_>,
  ctx: &mut ExecutionContext,
) -> Result<JobResult, RunnerError> {
  let Inputs {
    msg,
    config,
    cancel,
    events,
    workspace,
    http,
    containers,
  } = inputs;
  let fetcher = fetcher_for(msg, config, ctx, events)?;
  // A failed prefetch never fails the job; step-time resolution can retry.
  let prefetch_handle = spawn_prefetch(
    &msg.steps,
    Arc::clone(&fetcher),
    http.clone(),
    config.data_dir.clone(),
  );
  let shadow = build_shadow_observer(config, &msg.job_id, ctx);
  emit_job_started(events, &msg.job_id, &msg.job_display_name).await;

  // Later run mappings replace earlier workflow mappings, matching GitHub.
  let spec = JobSpec {
    acquired_outputs: msg.job_outputs.clone(),
    defaults: JobSpec::from_message_defaults(&msg.defaults, ctx)?,
  };
  let body = JobBody {
    msg,
    config,
    cancel,
    events,
    workspace,
    spec: &spec,
    shadow: &shadow,
    http,
    fetcher: fetcher.as_ref(),
  };
  let result = match prepare_before_steps(&body, ctx).await {
    Ok(Some(early)) => Ok(early),
    Ok(None) => match start_containers(&body, containers, ctx).await {
      Ok(()) => run_job_body(&body, ctx).await,
      Err(error) => Err(error),
    },
    Err(error) => Err(error),
  };
  // The fetch task may still be extracting into a private staging directory.
  // Aborting only stops future fetches; its atomic rename remains safe.
  prefetch_handle.abort();
  result
}

fn fetcher_for(
  msg: &AgentJobRequestMessage,
  config: &RunnerConfig,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) -> Result<Arc<ActionFetcher>, RunnerError> {
  let force = ctx
    .cancellation
    .as_ref()
    .map_or_else(CancellationToken::new, |signal| signal.force.clone());
  Ok(Arc::new(
    ActionFetcher::for_job(msg, Arc::clone(ctx.masker()), force, &config.network)?
      .with_events(events.clone()),
  ))
}

/// Keep job-started hooks and image setup ahead of every workflow step.
async fn prepare_before_steps(
  body: &JobBody<'_>,
  ctx: &mut ExecutionContext,
) -> Result<Option<JobResult>, RunnerError> {
  let started = run_job_hook(
    JobHookStage::Started,
    ctx,
    body.events,
    body.workspace,
    body.cancel,
  )
  .await?;
  if matches!(started, Some(Conclusion::Failure | Conclusion::Cancelled)) {
    return Ok(Some(JobResult {
      conclusion: started.unwrap_or(Conclusion::Failure),
      outputs: HashMap::new(),
      environment_url: None,
    }));
  }
  let result = docker_prepare::prepare(
    &body.msg.steps,
    docker_prepare::Discovery {
      fetcher: body.fetcher,
      client: body.http,
      data_dir: &body.config.data_dir,
    },
    ctx,
    body.events,
    body.cancel,
  )
  .await;
  let Err(error) = result else { return Ok(None) };
  Ok(Some(finish_preparation_error(body, ctx, error).await))
}

/// Run "Initialize containers". A failed or cancelled start does not end the
/// job: as upstream, the step loop still runs (conditions skip `success()`
/// steps), a step that runs fails without its container, then "Stop
/// containers" and "Complete job".
async fn start_containers(
  body: &JobBody<'_>,
  specs: (Option<&ContainerSpec>, &[ServiceSpec]),
  ctx: &mut ExecutionContext,
) -> Result<(), RunnerError> {
  let started = initialize_containers(
    specs,
    body.config,
    body.workspace,
    ctx,
    body.cancel,
    body.events,
  )
  .await?;
  match started {
    None | Some(Conclusion::Success) => {},
    Some(conclusion) => {
      if conclusion == Conclusion::Cancelled {
        ctx.record_job_cancelled();
      } else {
        ctx.record_step_failure();
      }
      ctx.container_setup = Some(conclusion);
      ctx.job_container_missing = specs.0.is_some();
    },
  }
  Ok(())
}

async fn finish_preparation_error(
  body: &JobBody<'_>,
  ctx: &mut ExecutionContext,
  error: RunnerError,
) -> JobResult {
  let conclusion = if matches!(error, RunnerError::Cancelled) || body.cancel.is_cancelled() {
    ctx.record_job_cancelled();
    Conclusion::Cancelled
  } else {
    ctx.record_step_failure();
    Conclusion::Failure
  };
  if conclusion != Conclusion::Cancelled {
    emit_log(
      body.events,
      shared::SETUP_STEP_ID,
      &format!("##[error]{error}"),
    )
    .await;
  }
  run_completed_hook_best_effort(ctx, body.events, body.workspace, body.cancel).await;
  JobResult {
    conclusion,
    outputs: HashMap::new(),
    environment_url: None,
  }
}
