use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[path = "execution_loop_logs.rs"]
mod logs;
#[path = "setup_forwarding.rs"]
mod setup_forwarding;
use logs::{forward_log_line, spawn_job_log_upload, spawn_step_uploader};

use super::SessionCtx;
use super::helpers::{RenewalParams, resolve_backend_ids, spawn_renewal};
use super::log_spool::LogSpool;
use super::outage_override::RenewalLoss;
use super::setup_step::report_setup_step;
use super::step_report_queue::{
  self, DRAIN_DEADLINE, StepMetaMap, StepReportQueue, StepReportQueueConfig,
};
use super::step_reporter::StepCollector;
use execution::Runner;
use shared::SecretMasker;
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerEvent};
use wire::reporting::live_log::{LiveLogLine, LiveLogStreamer};

/// Per-job addressing for the Run / Results services: where to renew
/// and report, with which token, under which plan.
pub(super) struct JobRoute<'a> {
  pub(super) run_service_url: &'a str,
  pub(super) rs_token: &'a str,
  pub(super) plan_id: &'a str,
}

/// Result of running one acquired job to completion.
///
/// `conclusion` and `annotations` may have been overridden by the outage
/// watchdog's failure-override path (see
/// [`apply_outage_override`](super::outage_override::apply_outage_override)) after
/// the renewal task was joined — the engine's own verdict is folded with
/// the watchdog's trip flag before this struct is built.
pub(super) struct JobExecution {
  pub(super) conclusion: Conclusion,
  pub(super) outputs: HashMap<String, String>,
  pub(super) steps: Vec<wire::reporting::StepResult>,
  pub(super) annotations: Vec<wire::reporting::Annotation>,
  /// The engine's evaluated, secret-free `environment.url`.
  pub(super) environment_url: Option<String>,
  /// First infrastructure category of a step that ended failed.
  pub(super) infrastructure_failure_category: Option<String>,
  /// The live-log wrapper task's `JoinHandle`, produced by [`connect_live_log`]
  /// inside the same `tokio::join!` that runs [`report_setup_step`] — threaded
  /// up so `job_lifecycle::run_acquired_job` can carry it into its
  /// `JobOutcome` for `poll_and_execute` to stash on `ctx` before any further
  /// fallible call (see the design note at that assignment site).
  pub(super) live_log_handle: Option<tokio::task::JoinHandle<()>>,
  /// The combined job-log upload task's `JoinHandle` (see
  /// [`spawn_job_log_upload`]), travelling the exact same route as
  /// `live_log_handle`: up through `JobOutcome`, onto
  /// `SessionCtx::job_log_upload`, joined by `helpers::cleanup_session` on
  /// both the `Ok` and `Err` paths of `job_lifecycle::poll_and_execute`.
  pub(super) job_log_upload: Option<tokio::task::JoinHandle<()>>,
}

/// Run setup reporting and the live-log handshake concurrently before starting
/// the engine. Both futures are boxed because the join otherwise makes this
/// function's future too large at its timeout-wrapped call sites.
pub(super) async fn execute_with_renewal(
  ctx: &SessionCtx,
  route: &JobRoute<'_>,
  job_msg: &AgentJobRequestMessage,
  job_cancel: &CancellationToken,
) -> JobExecution {
  let JobRoute {
    rs_token, plan_id, ..
  } = *route;
  let renewal_cancel = CancellationToken::new();
  let renewal_loss = Arc::new(AtomicU8::new(RenewalLoss::None as u8));
  let renewal_handle = start_renewal(
    ctx,
    route,
    job_msg,
    &renewal_cancel,
    job_cancel,
    Arc::clone(&renewal_loss),
  );

  // The helper boxes both concurrent legs to bound this timeout-wrapped future.
  let (setup_id, setup_lines, live_log_tx, live_log_handle) =
    connect_and_report_setup(ctx, job_msg, rs_token, plan_id).await;

  let collector = StepCollector::new();
  let mut cfg = build_fwd_config(ctx, rs_token, plan_id, job_msg, setup_lines, live_log_tx);
  cfg.setup_id = Some(setup_id);

  let outcome = run_forwarded_job(ctx, job_msg, &collector, cfg, job_cancel).await;
  renewal_cancel.cancel();
  let _ = renewal_handle.await;

  build_job_execution(
    outcome,
    &collector,
    RenewalLoss::from_raw(renewal_loss.load(Ordering::SeqCst)),
    live_log_handle,
  )
  .await
}

async fn build_job_execution(
  outcome: ForwarderOutcome,
  collector: &StepCollector,
  renewal_loss: RenewalLoss,
  live_log_handle: Option<tokio::task::JoinHandle<()>>,
) -> JobExecution {
  let ForwarderOutcome {
    conclusion,
    outputs,
    environment_url,
    job_log_upload,
  } = outcome;
  let (conclusion, annotations) =
    super::outage_override::apply_outage_override(conclusion, renewal_loss);
  let steps = collector.collected_results().await;
  JobExecution {
    conclusion,
    outputs,
    steps,
    annotations,
    environment_url,
    infrastructure_failure_category: collector.infrastructure_category().await,
    live_log_handle,
    job_log_upload,
  }
}

/// Start setup-step reporting and live-log connection at the same time.
/// The engine starts after both finish, preserving step number and log order.
async fn connect_and_report_setup(
  ctx: &SessionCtx,
  job_msg: &AgentJobRequestMessage,
  rs_token: &str,
  plan_id: &str,
) -> (
  String,
  Vec<String>,
  Option<mpsc::Sender<LiveLogLine>>,
  Option<tokio::task::JoinHandle<()>>,
) {
  {
    let mut masker = ctx
      .masker
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    super::setup_step::register_masks(job_msg, &mut masker);
  }
  // Box both legs to keep the enclosing future below clippy's size limit.
  let ((setup_id, mut setup_lines), (live_log_tx, live_log_handle)) = tokio::join!(
    Box::pin(report_setup_step(rs_token, plan_id, job_msg, &ctx.client)),
    Box::pin(connect_live_log(&ctx.config.network, job_msg, rs_token)),
  );
  setup_lines.push(format!(
    "Runner name: '{}'",
    super::setup_step::available(ctx.runner_name.as_deref())
  ));
  (setup_id, setup_lines, live_log_tx, live_log_handle)
}

/// Connect the live-log WebSocket, returning `(None, None)` on failure.
/// The wrapper handle reaches session cleanup through [`JobExecution`].
async fn connect_live_log(
  network: &shared::NetworkConfig,
  job_msg: &AgentJobRequestMessage,
  fallback_token: &str,
) -> (
  Option<tokio::sync::mpsc::Sender<LiveLogLine>>,
  Option<tokio::task::JoinHandle<()>>,
) {
  let Some(url) = job_msg.feed_stream_url() else {
    return (None, None);
  };
  let token = super::helpers::system_vss_access_token(job_msg);
  let ws_token = token.as_deref().unwrap_or(fallback_token);
  // HTTP/1.1 only, same `[network]` proxy / CA as every other client (#94).
  let client = match wire::reporting::live_log::live_log_client(network) {
    Ok(client) => client,
    Err(e) => {
      tracing::warn!(error = %e, "live log disabled: cannot build its HTTP client");
      return (None, None);
    },
  };
  let Some((tx, inner_handle)) = LiveLogStreamer::connect(&client, &url, ws_token).await else {
    return (None, None);
  };
  let wrapper_handle = tokio::spawn(async move {
    if let Err(e) = inner_handle.await {
      tracing::warn!(error = %e, "live log WebSocket task panicked");
    }
  });
  (Some(tx), Some(wrapper_handle))
}

/// Build the forwarder config from the session context and job, deriving
/// the Results Service URL and the run/job backend ids.
fn build_fwd_config(
  ctx: &SessionCtx,
  rs_token: &str,
  plan_id: &str,
  job_msg: &AgentJobRequestMessage,
  setup_lines: Vec<String>,
  live_log_tx: Option<tokio::sync::mpsc::Sender<LiveLogLine>>,
) -> FwdConfig {
  let results_url = job_msg
    .variables
    .get("system.github.results_endpoint")
    .map(|v| v.value.trim_end_matches('/').to_owned());
  let (run_backend_id, job_backend_id) = resolve_backend_ids(job_msg, plan_id);
  FwdConfig {
    setup_cancel: CancellationToken::new(),
    data_dir: ctx.config.data_dir.clone(),
    results_url,
    results_client: ctx.client.clone(),
    results_token: rs_token.to_owned(),
    run_backend_id,
    job_backend_id,
    setup_lines,
    setup_id: None,
    live_log_tx,
    masker: Arc::clone(&ctx.masker),
  }
}

/// What the event forwarder hands back the moment the job's verdict is
/// final: the conclusion, plus the still-running combined job-log upload it
/// spawned (see [`spawn_job_log_upload`]).
///
/// Sent over the forwarder's oneshot BEFORE the forwarder task itself ends,
/// so the caller can proceed to `report_completion` while the upload is
/// still in flight.
struct ForwarderOutcome {
  conclusion: Conclusion,
  outputs: HashMap<String, String>,
  environment_url: Option<String>,
  /// `None` when no Results Service URL is configured — there is nowhere to
  /// upload the combined log to, so no task was spawned.
  job_log_upload: Option<tokio::task::JoinHandle<()>>,
}

/// Run the engine and its event forwarder to completion, returning the
/// job conclusion. The engine owns its own event channel; we hand the
/// receiver to the forwarder, which derives the conclusion and signals
/// back via the oneshot.
async fn run_forwarded_job(
  ctx: &SessionCtx,
  job_msg: &AgentJobRequestMessage,
  collector: &StepCollector,
  mut cfg: FwdConfig,
  job_cancel: &CancellationToken,
) -> ForwarderOutcome {
  cfg.setup_cancel = job_cancel.clone();
  let job_log = prepare_job_log(
    &ctx.config.data_dir,
    &job_msg.job_id,
    cfg.results_url.is_some(),
  )
  .await;
  let runner = Runner::new(ctx.config.clone(), Arc::clone(&ctx.masker));
  let engine_rx = runner
    // The per-job token (child of the session token) so a mid-job
    // `JobCancellation` from the broker winds the engine down too.
    .execute_job_with_shutdown(job_msg.clone(), job_cancel.clone(), ctx.cancel.clone());

  let (outcome_tx, outcome_rx) = oneshot::channel::<ForwarderOutcome>();
  let fwd_handle = spawn_event_forwarder(
    engine_rx,
    collector.clone(),
    ctx.tx.clone(),
    cfg,
    outcome_tx,
    job_log,
  );

  let outcome = if let Ok(o) = outcome_rx.await {
    o
  } else {
    tracing::error!("event forwarder dropped the conclusion sender");
    ForwarderOutcome {
      conclusion: Conclusion::Failure,
      outputs: HashMap::new(),
      environment_url: None,
      job_log_upload: None,
    }
  };
  let _ = fwd_handle.await;
  outcome
}

async fn prepare_job_log(data_dir: &Path, job_id: &str, enabled: bool) -> Option<LogSpool> {
  if !enabled {
    return None;
  }
  let data_dir = data_dir.to_owned();
  let job_id = job_id.to_owned();
  match tokio::task::spawn_blocking(move || LogSpool::for_job(&data_dir, &job_id)).await {
    Ok(Ok(spool)) => Some(spool),
    Ok(Err(error)) => {
      tracing::warn!(error = %error, "job log spool unavailable");
      None
    },
    Err(error) => {
      tracing::warn!(error = %error, "job log spool task failed");
      None
    },
  }
}

/// Spawn the lock-renewal task, routing through `route` instead of three
/// loose strings (`run_service_url`/`rs_token`/`plan_id`) to leave headroom
/// under the crate's `too_many_arguments` cap for the watchdog wiring
/// (`job_cancel`, `outage_tripped`).
fn start_renewal(
  ctx: &SessionCtx,
  route: &JobRoute<'_>,
  job_msg: &AgentJobRequestMessage,
  cancel: &CancellationToken,
  job_cancel: &CancellationToken,
  renewal_loss: Arc<AtomicU8>,
) -> tokio::task::JoinHandle<()> {
  let params = RenewalParams {
    client: ctx.client.clone(),
    token: route.rs_token.to_owned(),
    run_service_url: route.run_service_url.to_owned(),
    plan_id: route.plan_id.to_owned(),
    job_id: job_msg.job_id.clone(),
    tx: ctx.tx.clone(),
    job_cancel: job_cancel.clone(),
    renewal_loss,
    outage_threshold: ctx.watchdog.outage_threshold,
    renew_interval: ctx.watchdog.renew_interval,
  };
  spawn_renewal(params, cancel.clone())
}

struct FwdConfig {
  /// Placeholder from `build_fwd_config`; `run_forwarded_job` rebinds it to
  /// the per-job token before the forwarder starts, so setup concludes as
  /// cancelled when the job was cancelled before its first step started.
  setup_cancel: CancellationToken,
  data_dir: PathBuf,
  /// `None` from `build_fwd_config`; the caller sets it to the reported
  /// "Set up job" step id once setup has been reported.
  setup_id: Option<String>,
  results_url: Option<String>,
  results_client: reqwest::Client,
  results_token: String,
  run_backend_id: String,
  job_backend_id: String,
  setup_lines: Vec<String>,
  live_log_tx: Option<tokio::sync::mpsc::Sender<LiveLogLine>>,
  /// Shared with the file sink's `MaskerRedactor` (via
  /// `init_with_redactor`) and the `ExecutionContext::register_secret`
  /// runtime path. Every `RunnerEvent::Log` line is passed through
  /// this masker before being pushed to the per-step streamer, the
  /// combined job log, or the live-log WebSocket. The file sink
  /// sees the same registration through the same Mutex, so a
  /// registration made via the runtime path is visible to all
  /// three downstream consumers on the very next line.
  masker: Arc<Mutex<SecretMasker>>,
}

/// Mask one log line, or one whole step-summary document, through the shared
/// `SecretMasker`.
///
/// Recovered from a poisoned Mutex the same way the production
/// `ExecutionContext::register_secret` and `MaskerRedactor::redact`
/// paths do — by extracting the inner `SecretMasker` via
/// `into_inner`. Centralized so a single test can pin the masking
/// contract for the forwarder.
fn mask_line(masker: &Arc<Mutex<SecretMasker>>, line: &str) -> String {
  match masker.lock() {
    Ok(g) => g.mask(line).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(line).into_owned(),
  }
}

/// Mutable per-job state threaded through the event forwarder.
///
/// Bundled so the per-event and finalize helpers take `&mut self`
/// instead of a long parameter list. Owns the running per-step
/// uploaders, the in-flight upload tasks, the spooled combined
/// job log, the step-report queue and its step-metadata cursor, and
/// the latched job conclusion.
struct ForwarderState {
  setup_pending: bool,
  step_meta: StepMetaMap,
  /// `None` when no Results Service URL is configured — matches the old
  /// `report_step_to_results`'s own `results_url` guard, now hoisted to a
  /// single check at construction instead of per event.
  step_queue: Option<StepReportQueue>,
  summary_queue: Option<super::step_summary::StepSummaryQueue>,
  uploaders: HashMap<String, mpsc::Sender<String>>,
  upload_tasks: tokio::task::JoinSet<Option<(String, String, u64)>>,
  job_log: Option<LogSpool>,
  conclusion: Option<Conclusion>,
  outputs: HashMap<String, String>,
  environment_url: Option<String>,
  /// Set once the live-log WebSocket streamer task has gone away
  /// (`try_send` returned `Closed`). Latches off further live sends so
  /// we stop spinning, and is logged exactly once. Durable logs are
  /// unaffected.
  live_log_closed: bool,
}

impl ForwarderState {
  /// Own the combined job spool and spawn the step-report queue when a Results Service URL is configured
  /// (see [`step_report_queue`]).
  fn new(cfg: &FwdConfig, job_log: Option<LogSpool>) -> Self {
    let step_queue = cfg
      .results_url
      .as_ref()
      .map(|url| spawn_step_queue(cfg, url));
    Self {
      setup_pending: cfg.setup_id.is_some(),
      step_meta: StepMetaMap::new(),
      step_queue,
      summary_queue: cfg.results_url.as_ref().map(|url| {
        super::step_summary::StepSummaryQueue::spawn(StepReportQueueConfig {
          client: cfg.results_client.clone(),
          results_url: url.clone(),
          token: cfg.results_token.clone(),
          run_backend_id: cfg.run_backend_id.clone(),
          job_backend_id: cfg.job_backend_id.clone(),
        })
      }),
      uploaders: HashMap::new(),
      upload_tasks: tokio::task::JoinSet::new(),
      job_log,
      conclusion: None,
      outputs: HashMap::new(),
      environment_url: None,
      live_log_closed: false,
    }
  }
}

/// Spawn the step-report queue task from the forwarder's config, discarding
/// its `JoinHandle` — `StepReportQueue::drain` synchronizes with the task's
/// own completion internally (see its doc comment), so the raw handle has
/// no further use here; dropping it does not abort the task.
fn spawn_step_queue(cfg: &FwdConfig, results_url: &str) -> StepReportQueue {
  let (queue, _handle) = StepReportQueue::spawn(
    StepReportQueueConfig {
      client: cfg.results_client.clone(),
      results_url: results_url.to_owned(),
      token: cfg.results_token.clone(),
      run_backend_id: cfg.run_backend_id.clone(),
      job_backend_id: cfg.job_backend_id.clone(),
    },
    DRAIN_DEADLINE,
  );
  queue
}

fn spawn_event_forwarder(
  mut events_rx: mpsc::Receiver<RunnerEvent>,
  fwd_collector: StepCollector,
  fwd_tx: mpsc::Sender<ListenerEvent>,
  mut cfg: FwdConfig,
  outcome_tx: oneshot::Sender<ForwarderOutcome>,
  job_log: Option<LogSpool>,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let setup_lines = std::mem::take(&mut cfg.setup_lines);
    let mut state = ForwarderState::new(&cfg, job_log);
    setup_forwarding::start(&mut state, &cfg, &fwd_collector, &fwd_tx, setup_lines).await;
    forward_events(&mut events_rx, &mut state, &cfg, &fwd_collector, &fwd_tx).await;
    finish_forwarder(state, &cfg, &fwd_collector, &fwd_tx, outcome_tx).await;
  })
}

async fn forward_events(
  events_rx: &mut mpsc::Receiver<RunnerEvent>,
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  collector: &StepCollector,
  tx: &mpsc::Sender<ListenerEvent>,
) {
  while let Some(mut event) = events_rx.recv().await {
    setup_forwarding::before_event(state, cfg, collector, tx, &mut event).await;
    if let RunnerEvent::JobCompleted {
      conclusion: c,
      outputs,
      environment_url,
      ..
    } = &event
    {
      state.conclusion = Some(*c);
      state.outputs.clone_from(outputs);
      state.environment_url.clone_from(environment_url);
    }
    if let RunnerEvent::InfrastructureError { message, .. } = &mut event {
      *message = mask_line(&cfg.masker, message);
    }
    collector.record(&event).await;
    handle_event_arm(state, cfg, &event).await;
    report_step(state, &event);
    if tx.send(ListenerEvent::Runner(event)).await.is_err() {
      break;
    }
  }
}

async fn finish_forwarder(
  mut state: ForwarderState,
  cfg: &FwdConfig,
  collector: &StepCollector,
  tx: &mpsc::Sender<ListenerEvent>,
  outcome_tx: oneshot::Sender<ForwarderOutcome>,
) {
  setup_forwarding::finish(&mut state, cfg, collector, tx, Conclusion::Failure).await;
  // Ordering is load-bearing: the per-step drain backfills the log URLs
  // `report_completion` ships, so it MUST finish first; the combined
  // job-log upload feeds nothing in that payload, so it only gets spawned.
  drain_step_uploads(&mut state, collector).await;
  let job_log_upload = spawn_job_log_upload(&mut state, cfg).await;
  if let Some(queue) = state.summary_queue.take() {
    queue.drain().await;
  }
  // Close the queue and wait (bounded) for its final flush BEFORE the
  // conclusion is sent — otherwise `complete_job` could race a step's
  // last-known status still sitting in the queue.
  if let Some(queue) = state.step_queue.take() {
    queue.drain().await;
  }
  if outcome_tx
    .send(ForwarderOutcome {
      conclusion: final_conclusion(&state),
      outputs: state.outputs,
      environment_url: state.environment_url,
      job_log_upload,
    })
    .is_err()
  {
    tracing::warn!("event forwarder conclusion receiver dropped before delivery");
  }
}

/// Dispatch the per-event side effects: spawn a step uploader on
/// `StepStarted`, forward log lines on `Log`, retire the uploader on
/// `StepCompleted`. Other events carry no forwarder-local side effect.
async fn handle_event_arm(state: &mut ForwarderState, cfg: &FwdConfig, event: &RunnerEvent) {
  match event {
    RunnerEvent::StepSummary { step_id, content } => {
      if let Some(queue) = &state.summary_queue {
        queue
          .enqueue(step_id, mask_line(&cfg.masker, content))
          .await;
      }
    },
    RunnerEvent::StepStarted {
      step_id, step_name, ..
    } => spawn_step_uploader(state, cfg, step_id, step_name),
    RunnerEvent::Log { step_id, line, .. } => forward_log_line(state, cfg, step_id, line).await,
    RunnerEvent::StepCompleted { step_id, .. } => {
      if cfg.setup_id.as_deref() != Some(step_id.as_str()) {
        state.uploaders.remove(step_id);
      }
    },
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::JobCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. }
    | RunnerEvent::StepMetadata { .. }
    | RunnerEvent::InfrastructureError { .. } => {},
  }
}

/// Enqueue the event's step-status update onto the step-report queue
/// (advancing the step-metadata cursor), when a Results Service is
/// configured. Never awaits the network — see
/// [`StepReportQueue::enqueue`].
fn report_step(state: &mut ForwarderState, event: &RunnerEvent) {
  let Some(ref queue) = state.step_queue else {
    return;
  };
  let Some(entry) = step_report_queue::build_step_entry(event, &mut state.step_meta) else {
    return;
  };
  queue.enqueue(entry);
}

/// Drain the in-flight per-step uploads, backfilling each step's log URL
/// and line count onto the already-collected `StepResult`s.
///
/// **Must complete before the conclusion is sent.** `collector.set_log_url`
/// is what populates `completed_log_url` / `completed_log_lines` on the
/// results `report_completion` ships in `CompleteJobRequest.step_results`;
/// a step reported without one renders as "log not found" in the GitHub UI.
/// That dependency is the whole reason this half is separate from
/// [`spawn_job_log_upload`], whose result nothing in the completion payload
/// reads.
async fn drain_step_uploads(state: &mut ForwarderState, collector: &StepCollector) {
  // Drop every per-step line sender FIRST. A step whose `StepCompleted`
  // never arrived (engine error mid-step) still has its sender parked in
  // `uploaders`, and its streamer task only finishes when that sender
  // drops — joining below without this clear deadlocks the forwarder
  // (live hang: job failed mid-step, runner never exited).
  state.uploaders.clear();
  // Drain UNCONDITIONALLY. A step that logged zero lines makes its uploader
  // return `Ok(None)`, and a panicked upload returns `Err(JoinError)` — both
  // must keep the loop going. A refutable `while let Some(Ok(Some(..)))` would
  // stop on the first such task, abandoning the remaining per-step uploads;
  // when the JoinSet then drops they are aborted mid-flight (after the blob PUT
  // but before `create_step_logs_metadata`), orphaning blobs so GitHub renders
  // "log not found" for those steps.
  while let Some(res) = state.upload_tasks.join_next().await {
    if let Ok(Some((step_id, log_url, line_count))) = res {
      collector.set_log_url(&step_id, log_url, line_count).await;
    }
  }
}

/// Resolve the latched conclusion, defaulting to failure if the engine
/// drained without ever emitting `JobCompleted`.
fn final_conclusion(state: &ForwarderState) -> Conclusion {
  state.conclusion.unwrap_or_else(|| {
    tracing::error!("forwarder drained engine without seeing JobCompleted");
    Conclusion::Failure
  })
}

#[cfg(test)]
#[path = "tests/execution_loop.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/multiline_mask.rs"]
mod multiline_mask;

#[cfg(test)]
#[path = "tests/step_summary_job.rs"]
mod step_summary_job;

#[cfg(test)]
#[path = "tests/job_outputs.rs"]
mod job_outputs;

#[cfg(test)]
#[path = "tests/setup_forwarding.rs"]
mod setup_tests;

#[cfg(test)]
#[path = "tests/execution_large_logs.rs"]
mod execution_large_logs;
