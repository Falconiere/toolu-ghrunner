//! Runs an initialized job with one action fetcher and its acquired defaults.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{JobBody, build_shadow_observer, emit_job_started, run_job_body};
use crate::execution::actions::prefetch::{ActionFetcher, spawn_prefetch};
use crate::execution::context::ExecutionContext;
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
}

pub(super) async fn execute(
  inputs: Inputs<'_>,
  ctx: &mut ExecutionContext,
) -> Result<(Conclusion, HashMap<String, String>), RunnerError> {
  let Inputs {
    msg,
    config,
    cancel,
    events,
    workspace,
    http,
  } = inputs;
  let fetcher = Arc::new(ActionFetcher::for_job(
    msg,
    Arc::clone(ctx.masker()),
    cancel.clone(),
  ));
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
    ..JobSpec::default()
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
  let result = run_job_body(&body, ctx).await;
  // The fetch task may still be extracting into a private staging directory.
  // Aborting only stops future fetches; its atomic rename remains safe.
  prefetch_handle.abort();
  result
}
