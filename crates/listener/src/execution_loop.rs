use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::helpers::{RenewalParams, ResultsCtx, resolve_backend_ids, spawn_renewal};
use super::log_uploader::StreamerConfig;
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
/// watchdog's failure-override path (see [`apply_outage_override`]) after
/// the renewal task was joined — the engine's own verdict is folded with
/// the watchdog's trip flag before this struct is built.
pub(super) struct JobExecution {
  pub(super) conclusion: Conclusion,
  pub(super) steps: Vec<wire::reporting::StepResult>,
  pub(super) annotations: Vec<wire::reporting::Annotation>,
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
  // Shared write-once trip flag: set by the renewal task's outage watchdog,
  // read below only after the renewal task is joined (no teardown race).
  let outage_tripped = Arc::new(AtomicBool::new(false));
  let renewal_handle = start_renewal(
    ctx,
    route,
    job_msg,
    &renewal_cancel,
    job_cancel,
    Arc::clone(&outage_tripped),
  );

  // "Set up job" (step 1, matches C# runner order — real workflow steps
  // start at number 2+) and the live-log WebSocket handshake have no mutual
  // dependency, so run them concurrently instead of paying the sum of their
  // round-trips. The engine (`run_forwarded_job` below) starts only once
  // BOTH have resolved. Each leg is boxed: `tokio::join!` holds both
  // futures' state for the join's own lifetime, and the two together
  // otherwise push this fn's returned future past clippy's `large_futures`
  // threshold (it is awaited inside a `tokio::time::timeout` at several
  // call sites, which inlines the whole state machine).
  let ((setup_result, setup_lines), (live_log_tx, live_log_handle)) = tokio::join!(
    Box::pin(report_setup_step(rs_token, plan_id, job_msg, &ctx.client)),
    Box::pin(connect_live_log(job_msg, rs_token)),
  );

  let collector = StepCollector::new();
  if let Some(result) = setup_result {
    collector.push_result(result).await;
  }
  let cfg = build_fwd_config(ctx, rs_token, plan_id, job_msg, setup_lines, live_log_tx);

  let ForwarderOutcome {
    conclusion,
    job_log_upload,
  } = run_forwarded_job(ctx, job_msg, &collector, cfg, job_cancel).await;
  renewal_cancel.cancel();
  let _ = renewal_handle.await;

  let (conclusion, annotations) =
    apply_outage_override(conclusion, outage_tripped.load(Ordering::SeqCst));
  let steps = collector.collected_results().await;
  JobExecution {
    conclusion,
    steps,
    annotations,
    live_log_handle,
    job_log_upload,
  }
}

/// Connect the live-log WebSocket for real-time log streaming to GitHub's
/// UI. Returns `(None, None)` on failure — live logs are best-effort, so a
/// connect failure never fails the job.
///
/// Run inside the same `tokio::join!` as [`report_setup_step`] (see
/// [`execute_with_renewal`]) — the two share no state, and previously ran
/// strictly in sequence (this fn's caller used to `.await` in full before
/// `report_setup_step` was ever called), paying the sum of both round-trips
/// instead of their max.
///
/// The second element of the returned tuple is the `JoinHandle` of the
/// wrapper task spawned below (not `LiveLogStreamer::connect`'s inner
/// handle) — threaded all the way up through [`JobExecution`] so
/// `job_lifecycle::poll_and_execute` can stash it on `ctx` for
/// `cleanup_session` to join, instead of it being dropped un-joined here.
async fn connect_live_log(
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
  let Some((tx, inner_handle)) = LiveLogStreamer::connect(&url, ws_token).await else {
    return (None, None);
  };
  let wrapper_handle = tokio::spawn(async move {
    if let Err(e) = inner_handle.await {
      tracing::warn!(error = %e, "live log WebSocket task panicked");
    }
  });
  (Some(tx), Some(wrapper_handle))
}

/// The single source of truth for the outage annotation text — referenced
/// by the `watchdog_trip` assertions so tests cannot drift from the
/// message actually reported to GitHub.
pub(crate) const LOST_CONNECTION_MESSAGE: &str =
  "Runner lost connection to GitHub for more than 5 minutes; job was cancelled (lost connection).";

/// Fold the outage watchdog's trip flag into the engine's conclusion.
///
/// Called only after the renewal task's `JoinHandle` has been awaited, so
/// there is no race between "the watchdog is still writing the flag" and
/// "we are reading it". A tripped flag overrides a non-`Success`
/// conclusion to `Failure` plus the "lost connection" annotation (an
/// honest verdict either way: a genuinely-failed step, or a GH-initiated
/// cancel racing the trip, both happened during a real outage). A tripped
/// flag alongside a `Success` conclusion can only be a
/// trip-during-teardown race — the job finished before the cancel
/// landed — so it is left as `Success`, WARN-logged once, with no
/// annotation; rewriting a successful job's history would be dishonest.
pub(crate) fn apply_outage_override(
  conclusion: Conclusion,
  outage_tripped: bool,
) -> (Conclusion, Vec<wire::reporting::Annotation>) {
  if !outage_tripped {
    return (conclusion, Vec::new());
  }
  if conclusion == Conclusion::Success {
    tracing::warn!(
      "outage watchdog tripped after the job already completed successfully \
       (trip-during-teardown race) — leaving conclusion as Success"
    );
    return (conclusion, Vec::new());
  }
  let annotation = wire::reporting::Annotation {
    annotation_type: "error".to_owned(),
    message: LOST_CONNECTION_MESSAGE.to_owned(),
    file: None,
    line: None,
    col: None,
  };
  (Conclusion::Failure, vec![annotation])
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

/// What the event forwarder hands back the moment the job's verdict is
/// final: the conclusion, plus the still-running combined job-log upload it
/// spawned (see [`spawn_job_log_upload`]).
///
/// Sent over the forwarder's oneshot BEFORE the forwarder task itself ends,
/// so the caller can proceed to `report_completion` while the upload is
/// still in flight.
struct ForwarderOutcome {
  conclusion: Conclusion,
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
  cfg: FwdConfig,
  job_cancel: &CancellationToken,
) -> ForwarderOutcome {
  let runner = Runner::new(ctx.config.clone(), Arc::clone(&ctx.masker));
  let engine_rx = runner
    // The per-job token (child of the session token) so a mid-job
    // `JobCancellation` from the broker winds the engine down too.
    .execute_job(job_msg.clone(), job_cancel.clone());

  let (outcome_tx, outcome_rx) = oneshot::channel::<ForwarderOutcome>();
  let fwd_handle = spawn_event_forwarder(
    engine_rx,
    collector.clone(),
    ctx.tx.clone(),
    cfg,
    outcome_tx,
  );

  let outcome = if let Ok(o) = outcome_rx.await {
    o
  } else {
    tracing::error!("event forwarder dropped the conclusion sender");
    ForwarderOutcome {
      conclusion: Conclusion::Failure,
      job_log_upload: None,
    }
  };
  let _ = fwd_handle.await;
  outcome
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
  outage_tripped: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
  let params = RenewalParams {
    client: ctx.client.clone(),
    token: route.rs_token.to_owned(),
    run_service_url: route.run_service_url.to_owned(),
    plan_id: route.plan_id.to_owned(),
    job_id: job_msg.job_id.clone(),
    tx: ctx.tx.clone(),
    job_cancel: job_cancel.clone(),
    outage_tripped,
    outage_threshold: ctx.watchdog.outage_threshold,
    renew_interval: ctx.watchdog.renew_interval,
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
    Ok(g) => g.mask(line).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(line).into_owned(),
  }
}

/// Mutable per-job state threaded through the event forwarder.
///
/// Bundled so the per-event and finalize helpers take `&mut self`
/// instead of a long parameter list. Owns the running per-step
/// uploaders, the in-flight upload tasks, the accumulated combined
/// job log, the step-report queue and its step-metadata cursor, and
/// the latched job conclusion.
struct ForwarderState {
  step_meta: StepMetaMap,
  /// `None` when no Results Service URL is configured — matches the old
  /// `report_step_to_results`'s own `results_url` guard, now hoisted to a
  /// single check at construction instead of per event.
  step_queue: Option<StepReportQueue>,
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
  /// Seed the combined job log with the "Set up job" step output, and
  /// spawn the step-report queue when a Results Service URL is configured
  /// (see [`step_report_queue`]).
  fn new(setup_lines: Vec<String>, cfg: &FwdConfig) -> Self {
    let step_queue = cfg
      .results_url
      .as_ref()
      .map(|url| spawn_step_queue(cfg, url));
    Self {
      step_meta: StepMetaMap::new(),
      step_queue,
      uploaders: HashMap::new(),
      upload_tasks: tokio::task::JoinSet::new(),
      all_job_lines: setup_lines,
      conclusion: None,
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
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let setup_lines = std::mem::take(&mut cfg.setup_lines);
    let mut state = ForwarderState::new(setup_lines, &cfg);
    while let Some(event) = events_rx.recv().await {
      if let RunnerEvent::JobCompleted { conclusion: c, .. } = &event {
        state.conclusion = Some(*c);
      }
      fwd_collector.record(&event).await;
      handle_event_arm(&mut state, &cfg, &event).await;
      report_step(&mut state, &event);
      if fwd_tx.send(ListenerEvent::Runner(event)).await.is_err() {
        break;
      }
    }
    // Ordering is load-bearing: the per-step drain backfills the log URLs
    // `report_completion` ships, so it MUST finish first; the combined
    // job-log upload feeds nothing in that payload, so it only gets spawned.
    drain_step_uploads(&mut state, &fwd_collector).await;
    let job_log_upload = spawn_job_log_upload(&mut state, &cfg);
    // Close the queue and wait (bounded) for its final flush BEFORE the
    // conclusion is sent — otherwise `complete_job` could race a step's
    // last-known status still sitting in the queue.
    if let Some(queue) = state.step_queue.take() {
      queue.drain().await;
    }
    let _ = outcome_tx.send(ForwarderOutcome {
      conclusion: final_conclusion(&state),
      job_log_upload,
    });
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

/// Spawn the combined job-level log upload (signed URL → blob PUT →
/// metadata, 3 round-trips) and hand back its `JoinHandle`.
///
/// Deliberately NOT awaited here. Nothing in `CompleteJobRequest` reads its
/// result — unlike [`drain_step_uploads`], which backfills every step's
/// `completed_log_url` — so it overlaps `report_completion` instead of
/// preceding it. The handle rides `JobExecution` → `JobOutcome` →
/// `SessionCtx::job_log_upload`, and `helpers::cleanup_session` awaits it on
/// both the `Ok` and the `Err` path of `job_lifecycle::poll_and_execute`, so
/// the listener still never returns before the upload finishes.
///
/// The task captures only owned Results-Service addressing plus the log
/// lines themselves — never a clone of the listener-event sender. A detached
/// task holding one would keep the journal channel open past `handler.rs`'s
/// `drop(ctx)` and wedge the `journal.await` that follows it.
fn spawn_job_log_upload(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
) -> Option<tokio::task::JoinHandle<()>> {
  let results_url = cfg.results_url.clone()?;
  let lines = std::mem::take(&mut state.all_job_lines);
  let client = cfg.results_client.clone();
  let token = cfg.results_token.clone();
  let run_backend_id = cfg.run_backend_id.clone();
  let job_backend_id = cfg.job_backend_id.clone();
  Some(tokio::spawn(async move {
    let rctx = ResultsCtx {
      client: &client,
      results_url: &results_url,
      token: &token,
      run_backend_id: &run_backend_id,
      job_backend_id: &job_backend_id,
    };
    if let Some(count) = super::log_uploader::upload_job_logs(&rctx, &lines).await {
      tracing::info!(line_count = count, "job log uploaded");
    }
  }))
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
