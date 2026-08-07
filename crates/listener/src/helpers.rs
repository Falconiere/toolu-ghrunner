//! Shared helpers for the listener lifecycle.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use crate::outage::OutageWatchdog;
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerError, RunnerEvent};
use wire::net::delete_session;
use wire::reporting::ReportConclusion;
use wire::reporting::run_service::{RenewJobRequest, renew_job};

/// Per-step metadata captured on `StepStarted` so later `StepCompleted` events
/// can emit a full Step record (actions/runner C# always sends both
/// `started_at` and `completed_at` on every update — null clobbers the value).
#[derive(Debug, Clone)]
pub(super) struct StepMeta {
  pub name: String,
  pub number: u32,
  pub started_at: String,
}

pub(super) type StepMetaMap = HashMap<String, StepMeta>;

/// Bundled context for Results Service calls — groups the constant-per-job
/// parameters so `report_step_to_results` / `StepLogStreamer` stay within the
/// 6-arg clippy cap.
pub struct ResultsCtx<'a> {
  /// Shared HTTP client for the Results Service calls.
  pub client: &'a reqwest::Client,
  /// Base URL of the Results Service.
  pub results_url: &'a str,
  /// Bearer token authorizing the calls.
  pub token: &'a str,
  /// The job's run-level backend id.
  pub run_backend_id: &'a str,
  /// The job's job-level backend id.
  pub job_backend_id: &'a str,
}

/// Resolve Results Service backend IDs from job message variables (matches C# runner).
/// Falls back to `plan_id`/`job_id` for older GHES without these variables.
pub(super) fn resolve_backend_ids(
  job_msg: &AgentJobRequestMessage,
  plan_id: &str,
) -> (String, String) {
  let run_id = job_msg
    .variables
    .get("system.github.run_backend_id")
    .map_or_else(|| plan_id.to_owned(), |v| v.value.clone());
  let job_id = job_msg
    .variables
    .get("system.github.job_backend_id")
    .map_or_else(|| job_msg.job_id.clone(), |v| v.value.clone());
  (run_id, job_id)
}

/// Fixed lock-renewal cadence (no backoff), matching the C# runner's ~60 s
/// renewal beat. Prod default for [`WatchdogConfig::renew_interval`]; tests
/// inject milliseconds so the trip-and-report path runs sub-second.
pub(super) const RENEW_INTERVAL: Duration = Duration::from_secs(60);

/// Test-injectable timing knobs for the mid-job outage watchdog hosted in
/// [`spawn_renewal`]. Threaded through `SessionCtx` (prod uses the
/// `Default` impl); tests override both fields to run the trip-and-report
/// path at millisecond cadence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WatchdogConfig {
  pub(crate) outage_threshold: Duration,
  pub(crate) renew_interval: Duration,
}

impl Default for WatchdogConfig {
  fn default() -> Self {
    Self {
      outage_threshold: crate::outage::OUTAGE_THRESHOLD,
      renew_interval: RENEW_INTERVAL,
    }
  }
}

/// Owned parameters for the lock renewal background task.
pub(super) struct RenewalParams {
  pub(super) client: reqwest::Client,
  pub(super) token: String,
  pub(super) run_service_url: String,
  pub(super) plan_id: String,
  pub(super) job_id: String,
  pub(super) tx: mpsc::Sender<ListenerEvent>,
  /// The job's own cancellation token (a child of the session token) —
  /// cancelled on an outage trip so the engine tears the running step
  /// down via `step_timeout::wait_bounded`.
  pub(super) job_cancel: CancellationToken,
  /// Write-once trip flag: set exactly once by the watchdog latch and
  /// never cleared afterward — even a later successful renewal must not
  /// un-trip it, since the job was already cancelled and the "lost
  /// connection" annotation (applied downstream) must survive.
  pub(super) outage_tripped: Arc<AtomicBool>,
  /// `OutageWatchdog` threshold: prod `outage::OUTAGE_THRESHOLD` (5 min),
  /// tests inject milliseconds.
  pub(super) outage_threshold: Duration,
  /// Lock-renewal cadence: prod `RENEW_INTERVAL` (60 s), tests inject
  /// milliseconds.
  pub(super) renew_interval: Duration,
}

pub(super) fn spawn_renewal(
  params: RenewalParams,
  cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let req = RenewJobRequest {
      plan_id: params.plan_id,
      job_id: params.job_id,
    };
    let mut watchdog = OutageWatchdog::new(Instant::now(), params.outage_threshold);
    let mut interval = tokio::time::interval(params.renew_interval);
    interval.tick().await; // skip first immediate tick
    loop {
      tokio::select! {
        biased;
        () = cancel.cancelled() => break,
        _ = interval.tick() => {
          match renew_job(&params.client, &params.run_service_url, &params.token, &req).await {
            Ok(resp) => {
              let _ = params.tx.send(ListenerEvent::LockRenewed { locked_until: resp.locked_until }).await;
              watchdog.record_ok(Instant::now());
            },
            Err(e) => {
              tracing::warn!(error = %e, "lock renewal failed");
              // Feed rule: only Network-class renewal failures (transport /
              // 429 / 5xx — GitHub unreachable) count toward the outage
              // window. A persistent Protocol/Auth error (definitive, e.g.
              // a stale 401/404) must never manufacture a false "lost
              // connection" trip.
              if matches!(e, RunnerError::Network(_)) && watchdog.record_err(Instant::now()) {
                params.outage_tripped.store(true, Ordering::SeqCst);
                params.job_cancel.cancel();
                let outage_secs = params.outage_threshold.as_secs();
                tracing::error!(
                  outage_secs,
                  "lost connection to GitHub for more than {outage_secs}s of renewal failures — job cancelled"
                );
              }
            },
          }
        },
      }
    }
  })
}

pub(super) fn map_conclusion(c: Conclusion) -> ReportConclusion {
  match c {
    Conclusion::Success => ReportConclusion::Success,
    Conclusion::Failure => ReportConclusion::Failure,
    Conclusion::Cancelled => ReportConclusion::Cancelled,
    Conclusion::Skipped => ReportConclusion::Skipped,
  }
}

/// Look up the `SystemVssConnection` endpoint's `AccessToken` from a job
/// request message.
///
/// Used by both the live-log WebSocket connection (which authenticates the
/// streaming channel) and the run-service token exchange (which authenticates
/// the Twirp RPCs). Both sites must agree on which endpoint supplies the
/// bearer token; this helper is the single chokepoint that picks the
/// `SystemVssConnection` endpoint case-insensitively and reads the
/// `AccessToken` parameter case-insensitively.
///
/// Returns `None` if no such endpoint or parameter is present.
pub(super) fn system_vss_access_token(job_msg: &AgentJobRequestMessage) -> Option<String> {
  job_msg
    .resources
    .endpoints
    .iter()
    .find(|e| e.name.eq_ignore_ascii_case("SystemVssConnection"))
    .and_then(|e| e.authorization.as_ref())
    .and_then(|a| {
      a.parameters
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("AccessToken"))
        .map(|(_, v)| v.clone())
    })
}

/// Delete the broker session and join the live-log flush, concurrently.
///
/// No artificial delay: `delete_session` already treats any non-2xx as
/// expected (the broker may already have forgotten about JIT runners), and
/// the live-log wrapper task (stashed on `ctx.live_log` by
/// `job_lifecycle::poll_and_execute`) is joined with a bounded 2s timeout
/// instead of the fixed 5s sleep this replaces. An elapsed timeout is
/// WARN-and-continue: the join handle is simply dropped, which does **not**
/// abort the spawned task — a genuinely stuck streamer can still be cut
/// short if the process exits (e.g. `--once`) before it finishes, the same
/// exposure the old sleep had, now bounded and logged here instead of paid
/// on every job.
pub(super) async fn cleanup_session(ctx: &mut SessionCtx) {
  // Take the handle out before building the two futures: `flush` needs to
  // own it (an `.await` consumes a `JoinHandle`), and `del` only needs the
  // other `ctx.*` fields — splitting this way avoids a mutable-borrow /
  // immutable-borrow conflict on `ctx` between the two joined futures.
  let live_log = ctx.live_log.take();
  let flush = async {
    if let Some(handle) = live_log
      && tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .is_err()
    {
      tracing::warn!("live-log flush timed out; durable logs unaffected");
    }
  };
  let del = delete_session(&ctx.client, &ctx.broker_url, &ctx.token, &ctx.session_id);
  let ((), delete_result) = tokio::join!(flush, del);
  if let Err(e) = delete_result {
    tracing::debug!(error = %e, "session delete failed (non-fatal)");
  }
}

/// Report a step event to GitHub's Results Service. Errors are logged, not propagated.
pub(super) async fn report_step_to_results(
  rctx: &ResultsCtx<'_>,
  event: &RunnerEvent,
  change_order: &mut u64,
  step_meta: &mut StepMetaMap,
) {
  use wire::reporting::results_service::{WorkflowStepsUpdateRequest, update_workflow_steps};

  let Some(entry) = build_step_entry(event, step_meta) else {
    return;
  };

  let order = *change_order;
  *change_order += 1;

  let request = WorkflowStepsUpdateRequest {
    steps: vec![entry],
    change_order: order,
    workflow_run_backend_id: rctx.run_backend_id.to_owned(),
    workflow_job_run_backend_id: rctx.job_backend_id.to_owned(),
  };

  let prefix_len = std::cmp::min(10, rctx.token.len());
  let token_prefix = rctx.token.get(..prefix_len).map_or("", |s| s);

  if let Err(e) = update_workflow_steps(rctx.client, rctx.results_url, rctx.token, &request).await {
    tracing::warn!(
      error = %e,
      token_prefix,
      url = rctx.results_url,
      run_backend_id = rctx.run_backend_id,
      job_backend_id = rctx.job_backend_id,
      "results service step update failed"
    );
  }
}

/// Build the `StepUpdateEntry` for an event, updating `step_meta` as needed.
/// Returns `None` for events that don't map to a step update.
fn build_step_entry(
  event: &RunnerEvent,
  step_meta: &mut StepMetaMap,
) -> Option<wire::reporting::results_service::StepUpdateEntry> {
  use wire::reporting::Status;
  use wire::reporting::results_service::StepUpdateEntry;

  match event {
    RunnerEvent::StepStarted {
      step_id,
      step_name,
      step_number,
    } => {
      let started_at = chrono::Utc::now().to_rfc3339();
      step_meta.insert(
        step_id.clone(),
        StepMeta {
          name: step_name.clone(),
          number: *step_number,
          started_at: started_at.clone(),
        },
      );
      Some(StepUpdateEntry {
        external_id: step_id.clone(),
        number: *step_number,
        name: step_name.clone(),
        status: Status::InProgress,
        conclusion: None,
        started_at: Some(started_at),
        completed_at: None,
      })
    },
    RunnerEvent::StepCompleted {
      step_id,
      conclusion,
      ..
    } => {
      let meta = step_meta.get(step_id).cloned().unwrap_or(StepMeta {
        name: String::new(),
        number: 0,
        started_at: chrono::Utc::now().to_rfc3339(),
      });
      Some(StepUpdateEntry {
        external_id: step_id.clone(),
        number: meta.number,
        name: meta.name,
        status: Status::Completed,
        conclusion: Some(map_conclusion(*conclusion)),
        started_at: Some(meta.started_at),
        completed_at: Some(chrono::Utc::now().to_rfc3339()),
      })
    },
    RunnerEvent::JobStarted { .. }
    | RunnerEvent::JobCompleted { .. }
    | RunnerEvent::StepSkipped { .. }
    | RunnerEvent::Log { .. }
    | RunnerEvent::LogGroup { .. }
    | RunnerEvent::Annotation { .. } => None,
  }
}

#[cfg(test)]
#[path = "tests/helpers.rs"]
mod tests;
