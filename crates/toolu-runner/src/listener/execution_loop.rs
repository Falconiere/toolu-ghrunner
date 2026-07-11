use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::helpers::{
  RenewalParams, ResultsCtx, StepMetaMap, report_step_to_results, resolve_backend_ids,
  spawn_renewal,
};
use super::log_uploader::StreamerConfig;
use super::setup_step::report_setup_step;
use super::step_reporter::StepCollector;
use crate::Runner;
use crate::reporting::live_log::LiveLogLine;
use shared::SecretMasker;
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerEvent};

/// Per-job addressing for the Run / Results services: where to renew
/// and report, with which token, under which plan.
pub(super) struct JobRoute<'a> {
  pub(super) run_service_url: &'a str,
  pub(super) rs_token: &'a str,
  pub(super) plan_id: &'a str,
}

pub(super) async fn execute_with_renewal(
  ctx: &SessionCtx,
  route: &JobRoute<'_>,
  job_msg: &AgentJobRequestMessage,
  live_log_tx: Option<tokio::sync::mpsc::Sender<LiveLogLine>>,
  job_cancel: &CancellationToken,
) -> (Conclusion, Vec<crate::reporting::StepResult>) {
  let JobRoute {
    run_service_url,
    rs_token,
    plan_id,
  } = *route;
  let renewal_cancel = CancellationToken::new();
  let renewal_handle = start_renewal(
    ctx,
    run_service_url,
    rs_token,
    plan_id,
    job_msg,
    &renewal_cancel,
  );

  // Report "Set up job" as step 1 (matches C# runner order).
  // Real workflow steps start at number 2+.
  let (setup_result, setup_lines) =
    report_setup_step(rs_token, plan_id, job_msg, &ctx.client).await;

  let collector = StepCollector::new();
  if let Some(result) = setup_result {
    collector.push_result(result).await;
  }
  let cfg = build_fwd_config(ctx, rs_token, plan_id, job_msg, setup_lines, live_log_tx);

  let conclusion = run_forwarded_job(ctx, job_msg, &collector, cfg, job_cancel).await;
  renewal_cancel.cancel();
  let _ = renewal_handle.await;

  let step_results = collector.collected_results().await;
  (conclusion, step_results)
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
    results_url,
    results_client: ctx.client.clone(),
    results_token: rs_token.to_owned(),
    run_backend_id,
    job_backend_id,
    setup_lines,
    live_log_tx,
    masker: Arc::clone(&ctx.masker),
  }
}

/// Run the engine and its event forwarder to completion, returning the
/// job conclusion. The engine owns its own event channel; we hand the
/// receiver to the forwarder, which derives the conclusion and signals
/// back via the oneshot.
async fn run_forwarded_job(
  ctx: &SessionCtx,
  job_msg: &AgentJobRequestMessage,
  collector: &StepCollector,
  cfg: FwdConfig,
  job_cancel: &CancellationToken,
) -> Conclusion {
  let runner = Runner::new(ctx.config.clone(), Arc::clone(&ctx.masker));
  let engine_rx = runner
    // The per-job token (child of the session token) so a mid-job
    // `JobCancellation` from the broker winds the engine down too.
    .execute_job(job_msg.clone(), job_cancel.clone())
    .await;

  let (conclusion_tx, conclusion_rx) = oneshot::channel::<Conclusion>();
  let fwd_handle = spawn_event_forwarder(
    engine_rx,
    collector.clone(),
    ctx.tx.clone(),
    cfg,
    conclusion_tx,
  );

  let conclusion = if let Ok(c) = conclusion_rx.await {
    c
  } else {
    tracing::error!("event forwarder dropped the conclusion sender");
    Conclusion::Failure
  };
  let _ = fwd_handle.await;
  conclusion
}

fn start_renewal(
  ctx: &SessionCtx,
  run_service_url: &str,
  rs_token: &str,
  plan_id: &str,
  job_msg: &AgentJobRequestMessage,
  cancel: &CancellationToken,
) -> tokio::task::JoinHandle<()> {
  let params = RenewalParams {
    client: ctx.client.clone(),
    token: rs_token.to_owned(),
    run_service_url: run_service_url.to_owned(),
    plan_id: plan_id.to_owned(),
    job_id: job_msg.job_id.clone(),
    tx: ctx.tx.clone(),
  };
  spawn_renewal(params, cancel.clone())
}

struct FwdConfig {
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

/// Mask a single log line through the shared `SecretMasker`.
///
/// Recovered from a poisoned Mutex the same way the production
/// `ExecutionContext::register_secret` and `MaskerRedactor::redact`
/// paths do — by extracting the inner `SecretMasker` via
/// `into_inner`. Centralized so a single test can pin the masking
/// contract for the forwarder.
fn mask_line(masker: &Arc<Mutex<SecretMasker>>, line: &str) -> String {
  match masker.lock() {
    Ok(g) => g.mask(line),
    Err(poisoned) => poisoned.into_inner().mask(line),
  }
}

/// Mutable per-job state threaded through the event forwarder.
///
/// Bundled so the per-event and finalize helpers take `&mut self`
/// instead of a long parameter list. Owns the running per-step
/// uploaders, the in-flight upload tasks, the accumulated combined
/// job log, the Results Service change-order/step-metadata cursors,
/// and the latched job conclusion.
struct ForwarderState {
  change_order: u64,
  step_meta: StepMetaMap,
  uploaders: HashMap<String, mpsc::Sender<String>>,
  upload_tasks: tokio::task::JoinSet<Option<(String, String, u64)>>,
  all_job_lines: Vec<String>,
  conclusion: Option<Conclusion>,
  /// Set once the live-log WebSocket streamer task has gone away
  /// (`try_send` returned `Closed`). Latches off further live sends so
  /// we stop spinning, and is logged exactly once. Durable logs are
  /// unaffected.
  live_log_closed: bool,
}

impl ForwarderState {
  /// Seed the combined job log with the "Set up job" step output so
  /// the uploaded job log includes setup lines.
  fn new(setup_lines: Vec<String>) -> Self {
    Self {
      change_order: 1,
      step_meta: StepMetaMap::new(),
      uploaders: HashMap::new(),
      upload_tasks: tokio::task::JoinSet::new(),
      all_job_lines: setup_lines,
      conclusion: None,
      live_log_closed: false,
    }
  }
}

fn spawn_event_forwarder(
  mut events_rx: mpsc::Receiver<RunnerEvent>,
  fwd_collector: StepCollector,
  fwd_tx: mpsc::Sender<ListenerEvent>,
  mut cfg: FwdConfig,
  conclusion_tx: oneshot::Sender<Conclusion>,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let mut state = ForwarderState::new(std::mem::take(&mut cfg.setup_lines));
    while let Some(event) = events_rx.recv().await {
      if let RunnerEvent::JobCompleted { conclusion: c, .. } = &event {
        state.conclusion = Some(*c);
      }
      fwd_collector.record(&event).await;
      handle_event_arm(&mut state, &cfg, &event).await;
      report_step(&mut state, &cfg, &event).await;
      if fwd_tx.send(ListenerEvent::Runner(event)).await.is_err() {
        break;
      }
    }
    finalize_job_logs(&mut state, &cfg, &fwd_collector).await;
    let _ = conclusion_tx.send(final_conclusion(&state));
  })
}

/// Dispatch the per-event side effects: spawn a step uploader on
/// `StepStarted`, forward log lines on `Log`, retire the uploader on
/// `StepCompleted`. Other events carry no forwarder-local side effect.
async fn handle_event_arm(state: &mut ForwarderState, cfg: &FwdConfig, event: &RunnerEvent) {
  match event {
    RunnerEvent::StepStarted {
      step_id, step_name, ..
    } => spawn_step_uploader(state, cfg, step_id, step_name),
    RunnerEvent::Log { step_id, line, .. } => forward_log_line(state, cfg, step_id, line).await,
    RunnerEvent::StepCompleted { step_id, .. } => {
      state.uploaders.remove(step_id);
    },
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::JobCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. } => {},
  }
}

/// Spawn a per-step log streamer (only when a Results Service URL is
/// configured), register its line sender, and track the upload task so
/// its log URL can be backfilled on drain.
fn spawn_step_uploader(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  step_id: &str,
  step_name: &str,
) {
  let Some(ref url) = cfg.results_url else {
    return;
  };
  let (tx, handle) = super::log_uploader::spawn(StreamerConfig {
    client: cfg.results_client.clone(),
    results_url: url.clone(),
    token: cfg.results_token.clone(),
    run_backend_id: cfg.run_backend_id.clone(),
    job_backend_id: cfg.job_backend_id.clone(),
    step_backend_id: step_id.to_owned(),
    step_name: step_name.to_owned(),
  });
  state.uploaders.insert(step_id.to_owned(), tx);
  let sid = step_id.to_owned();
  state.upload_tasks.spawn(async move {
    handle
      .await
      .ok()
      .flatten()
      .map(|(url, count)| (sid, url, count))
  });
}

/// Mask a log line once and fan it out to every consumer: the combined
/// job log, the per-step streamer, and the live-log WebSocket.
///
/// The live-log send is best-effort + NON-BLOCKING `try_send`: the feed
/// is network-bound and must never backpressure the job. A high-volume
/// step (e.g. a `cargo build` flood) can outrun the WS drain; if the
/// bounded channel is full we DROP this line from the live view only —
/// the durable step-log (pushed above) still carries every line.
async fn forward_log_line(state: &mut ForwarderState, cfg: &FwdConfig, step_id: &str, line: &str) {
  // The file sink's redactor runs on the same Mutex, so the
  // registration that put this secret into the masker is visible here.
  let masked = mask_line(&cfg.masker, line);
  state.all_job_lines.push(masked.clone());
  if let Some(tx) = state.uploaders.get(step_id) {
    let _ = tx.send(masked.clone()).await;
  }
  if let Some(ref live_tx) = cfg.live_log_tx
    && !state.live_log_closed
  {
    // Distinguish backpressure (Full → intended silent drop, the durable
    // logs above still carry the line) from a dead streamer (Closed → latch
    // off and WARN once so we neither spin nor flood the diag log).
    if let Err(mpsc::error::TrySendError::Closed(_)) = live_tx.try_send(LiveLogLine {
      step_id: step_id.to_owned(),
      line: masked,
    }) {
      tracing::warn!("live-log feed closed; durable logs unaffected");
      state.live_log_closed = true;
    }
  }
}

/// Forward the event to the Results Service step report, advancing the
/// change-order and step-metadata cursors. No-op without a Results URL.
async fn report_step(state: &mut ForwarderState, cfg: &FwdConfig, event: &RunnerEvent) {
  let Some(ref url) = cfg.results_url else {
    return;
  };
  let rctx = ResultsCtx {
    client: &cfg.results_client,
    results_url: url,
    token: &cfg.results_token,
    run_backend_id: &cfg.run_backend_id,
    job_backend_id: &cfg.job_backend_id,
  };
  report_step_to_results(&rctx, event, &mut state.change_order, &mut state.step_meta).await;
}

/// Drain the in-flight per-step uploads (backfilling each step's log
/// URL) and upload the combined job-level log blob.
async fn finalize_job_logs(state: &mut ForwarderState, cfg: &FwdConfig, collector: &StepCollector) {
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

  let Some(ref url) = cfg.results_url else {
    return;
  };
  let rctx = ResultsCtx {
    client: &cfg.results_client,
    results_url: url,
    token: &cfg.results_token,
    run_backend_id: &cfg.run_backend_id,
    job_backend_id: &cfg.job_backend_id,
  };
  if let Some(count) = super::log_uploader::upload_job_logs(&rctx, &state.all_job_lines).await {
    tracing::info!(line_count = count, "job log uploaded");
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
