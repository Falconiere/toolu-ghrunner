//! Deferred, batched step-status reporting to GitHub's Results Service.
//!
//! `StepStarted`/`StepCompleted` events used to `await` an inline
//! `update_workflow_steps` Twirp call directly inside the event-forwarder
//! loop (see the removed `helpers::report_step_to_results`) — a slow
//! Results Service stalled log draining and could back-pressure into the
//! running step. [`StepReportQueue`] moves that call off the forwarder's
//! hot path: [`StepReportQueue::enqueue`] never awaits the network (a push
//! onto an unbounded `mpsc`), and a dedicated task opportunistically
//! batches whatever is queued into ONE `update_workflow_steps` request per
//! flush — the request already carries `Vec<StepUpdateEntry>`.
//!
//! `change_order` becomes a per-**request** scalar (one value per flush,
//! not per entry): the counter is owned by the queue task, seeded to `1`
//! (the setup-step's own direct report hardcodes `0` — untouched by this
//! module). A failed flush WARNs and drops that batch, mirroring the old
//! per-call failure behavior; a step-report failure never feeds the
//! [`crate::outage::OutageWatchdog`] (only renewal `Network` errors do).

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use shared::{Conclusion, RunnerEvent};
use wire::reporting::Status;
use wire::reporting::results_service::{
  StepUpdateEntry, WorkflowStepsUpdateRequest, update_workflow_steps,
};

use crate::helpers::map_conclusion;

/// Production drain deadline: long enough to absorb one more flush against
/// a healthy endpoint, short enough that a stalled Results Service cannot
/// stall job completion past it. Tests inject a shorter deadline through
/// [`StepReportQueue::spawn`]'s parameter instead of waiting this out.
pub(crate) const DRAIN_DEADLINE: Duration = Duration::from_secs(10);

/// Per-step metadata captured on `StepStarted` so a later `StepCompleted`
/// can emit a full entry (actions/runner's C# always sends both
/// `started_at` and `completed_at` on every update — a null clobbers the
/// value already reported).
#[derive(Debug, Clone)]
pub(crate) struct StepMeta {
  /// The step's display name, as reported on `StepStarted`.
  pub(crate) name: String,
  /// The step's 1-based index within the job.
  pub(crate) number: u32,
  /// ISO 8601 timestamp the step started.
  pub(crate) started_at: String,
}

/// Map from step id to its captured `StepStarted` metadata, threaded
/// through the forwarder's per-job state so `StepCompleted` can recover
/// the name/number/start time.
pub(crate) type StepMetaMap = HashMap<String, StepMeta>;

/// Owned per-job Results Service routing the queue task needs to call
/// `update_workflow_steps` on its own, independent of the forwarder's
/// borrowed `FwdConfig` — the task outlives any single borrow of it.
pub(crate) struct StepReportQueueConfig {
  /// HTTP client used for the Results Service call.
  pub(crate) client: reqwest::Client,
  /// Base URL of the Results Service.
  pub(crate) results_url: String,
  /// Bearer token authorizing the call.
  pub(crate) token: String,
  /// The job's run-level backend id.
  pub(crate) run_backend_id: String,
  /// The job's job-level backend id.
  pub(crate) job_backend_id: String,
}

/// Handle for enqueueing step-status updates without ever awaiting the
/// network. A dedicated background task (started by [`StepReportQueue::spawn`])
/// owns the actual `update_workflow_steps` calls, batching whatever is
/// queued into one request per flush.
pub(crate) struct StepReportQueue {
  tx: mpsc::UnboundedSender<StepUpdateEntry>,
  done_rx: oneshot::Receiver<()>,
  drain_deadline: Duration,
}

impl StepReportQueue {
  /// Spawn the batching task and return a handle for enqueueing plus the
  /// task's own `JoinHandle`.
  ///
  /// `drain_deadline` bounds [`Self::drain`] — production passes
  /// [`DRAIN_DEADLINE`] (10s); tests pass a short deadline so a
  /// held-forever endpoint doesn't slow the suite down.
  pub(crate) fn spawn(
    cfg: StepReportQueueConfig,
    drain_deadline: Duration,
  ) -> (Self, JoinHandle<()>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (done_tx, done_rx) = oneshot::channel();
    let handle = tokio::spawn(run_queue(cfg, rx, done_tx));
    (
      Self {
        tx,
        done_rx,
        drain_deadline,
      },
      handle,
    )
  }

  /// Queue one step-status update. Never awaits the network — entries are
  /// tiny and bounded (at most ~2x the job's step count: one `StepStarted`
  /// plus one `StepCompleted` each), so an unbounded channel push is the
  /// whole cost, unlike a bounded channel's `send().await` which could
  /// itself back-pressure the forwarder.
  pub(crate) fn enqueue(&self, entry: StepUpdateEntry) {
    // A send error means the queue task already exited (drained, or
    // panicked) — unreachable in the forwarder's own lifecycle (the task
    // outlives every `enqueue` call until `drain` consumes `self`), but
    // handled rather than assumed so a future caller can't panic here.
    if self.tx.send(entry).is_err() {
      tracing::warn!("step report queue task is gone; dropping a step update");
    }
  }

  /// Close the queue (no more entries), wait for the task to flush
  /// whatever was already queued and exit, bounded by `drain_deadline`.
  ///
  /// Dropping `self`'s sender closes the channel: the task's `recv` loop
  /// still drains whatever was already queued (a real final flush) before
  /// it observes the close and exits, so this is not a bare "give up"
  /// point. On expiry — or if the task ended without signalling (e.g. a
  /// panic) — this WARNs and returns; any entries the task had not yet
  /// flushed are dropped from GitHub's perspective, mirroring today's
  /// per-call WARN-and-drop failure semantics, now also covering a
  /// stalled endpoint instead of stalling completion on it.
  pub(crate) async fn drain(self) {
    drop(self.tx);
    match tokio::time::timeout(self.drain_deadline, self.done_rx).await {
      Ok(Ok(())) => {},
      Ok(Err(_recv_error)) => {
        tracing::warn!(
          "step report queue task ended without signalling completion; \
           remaining step updates may have been dropped"
        );
      },
      Err(_elapsed) => {
        tracing::warn!(
          deadline_secs = self.drain_deadline.as_secs(),
          "step report queue drain timed out; remaining step updates dropped"
        );
      },
    }
  }
}

/// The queue task body: receive one entry (blocking), then opportunistically
/// drain everything else already queued via `try_recv` before flushing —
/// this is what turns M rapid transitions into fewer than M requests.
/// Exits (and signals `done_tx`) once the channel closes AND every queued
/// entry has been flushed.
async fn run_queue(
  cfg: StepReportQueueConfig,
  mut rx: mpsc::UnboundedReceiver<StepUpdateEntry>,
  done_tx: oneshot::Sender<()>,
) {
  let mut change_order: u64 = 1;
  while let Some(first) = rx.recv().await {
    let mut batch = vec![first];
    while let Ok(next) = rx.try_recv() {
      batch.push(next);
    }
    flush(&cfg, &mut change_order, batch).await;
  }
  // Best-effort: a dropped receiver on the other end just means `drain`
  // already gave up (deadline elapsed) and stopped listening.
  let _ = done_tx.send(());
}

/// Send one `update_workflow_steps` request for `steps`, advancing
/// `change_order`. A failure WARNs and drops the batch — mirrors the old
/// per-call behavior; never feeds the outage watchdog (renewal `Network`
/// errors only, per `helpers::spawn_renewal`).
async fn flush(cfg: &StepReportQueueConfig, change_order: &mut u64, steps: Vec<StepUpdateEntry>) {
  let order = *change_order;
  *change_order += 1;
  let batch_size = steps.len();

  let request = WorkflowStepsUpdateRequest {
    steps,
    change_order: order,
    workflow_run_backend_id: cfg.run_backend_id.clone(),
    workflow_job_run_backend_id: cfg.job_backend_id.clone(),
  };

  let prefix_len = std::cmp::min(10, cfg.token.len());
  let token_prefix = cfg.token.get(..prefix_len).map_or("", |s| s);

  if let Err(e) = update_workflow_steps(&cfg.client, &cfg.results_url, &cfg.token, &request).await {
    tracing::warn!(
      error = %e,
      token_prefix,
      url = cfg.results_url,
      run_backend_id = cfg.run_backend_id,
      job_backend_id = cfg.job_backend_id,
      change_order = order,
      batch_size,
      "results service step update batch failed"
    );
  }
}

/// Build the `StepUpdateEntry` for an event, updating `step_meta` as needed.
/// Returns `None` for events that don't map to a step update.
pub(crate) fn build_step_entry(
  event: &RunnerEvent,
  step_meta: &mut StepMetaMap,
) -> Option<StepUpdateEntry> {
  match event {
    RunnerEvent::StepStarted {
      step_id,
      step_name,
      step_number,
    } => Some(entry_for_started(
      step_meta,
      step_id,
      step_name,
      *step_number,
    )),
    RunnerEvent::StepCompleted {
      step_id,
      conclusion,
      ..
    } => Some(entry_for_completed(step_meta, step_id, *conclusion)),
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::JobCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::Log { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. } => None,
  }
}

/// Build the `StepUpdateEntry` for a `StepStarted` event, recording its
/// metadata in `step_meta` so the matching `StepCompleted` can recover it.
fn entry_for_started(
  step_meta: &mut StepMetaMap,
  step_id: &str,
  step_name: &str,
  step_number: u32,
) -> StepUpdateEntry {
  let started_at = chrono::Utc::now().to_rfc3339();
  step_meta.insert(
    step_id.to_owned(),
    StepMeta {
      name: step_name.to_owned(),
      number: step_number,
      started_at: started_at.clone(),
    },
  );
  StepUpdateEntry {
    external_id: step_id.to_owned(),
    number: step_number,
    name: step_name.to_owned(),
    status: Status::InProgress,
    conclusion: None,
    started_at: Some(started_at),
    completed_at: None,
  }
}

/// Build the `StepUpdateEntry` for a `StepCompleted` event, recovering the
/// name/number/start time captured on the matching `StepStarted` (falling
/// back to empty/zero/now if it was never seen).
fn entry_for_completed(
  step_meta: &StepMetaMap,
  step_id: &str,
  conclusion: Conclusion,
) -> StepUpdateEntry {
  let meta = step_meta.get(step_id).cloned().unwrap_or(StepMeta {
    name: String::new(),
    number: 0,
    started_at: chrono::Utc::now().to_rfc3339(),
  });
  StepUpdateEntry {
    external_id: step_id.to_owned(),
    number: meta.number,
    name: meta.name,
    status: Status::Completed,
    conclusion: Some(map_conclusion(conclusion)),
    started_at: Some(meta.started_at),
    completed_at: Some(chrono::Utc::now().to_rfc3339()),
  }
}

#[cfg(test)]
#[path = "tests/step_report_queue_unit.rs"]
mod tests;
