//! Shared helpers for the listener lifecycle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use crate::outage::OutageWatchdog;
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent};
use wire::net::delete_session;
use wire::reporting::ReportConclusion;
use wire::reporting::run_service::{RenewJobRequest, renew_job};

/// Bundled context for Results Service calls — groups the constant-per-job
/// parameters so `step_report_queue::flush` / `StepLogStreamer` stay within
/// the 6-arg clippy cap.
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

/// Renew the job lock and cancel the job when it is lost.
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
            Ok(Some(resp)) => {
              let _ = params.tx.send(ListenerEvent::LockRenewed { locked_until: resp.locked_until }).await;
              watchdog.record_ok(Instant::now());
            },
            Ok(None) => {
              params.outage_tripped.store(true, Ordering::SeqCst);
              params.job_cancel.cancel();
              tracing::error!("GitHub no longer recognizes this job — job cancelled");
              break;
            },
            Err(e) => {
              tracing::warn!(error = %e, "lock renewal failed");
              if watchdog.record_err(Instant::now()) {
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

/// Map an engine conclusion to its reporting value.
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

/// Delete the broker session while joining live and combined log uploads.
/// The live flush has a 2s limit; the durable combined upload joins without one.
pub(super) async fn cleanup_session(ctx: &mut SessionCtx) {
  // Take the handles out before building the futures: each `.await`
  // consumes its `JoinHandle`, and `del` only needs the other `ctx.*`
  // fields — splitting this way avoids a mutable-borrow / immutable-borrow
  // conflict on `ctx` between the joined futures.
  let live_log = ctx.live_log.take();
  let job_log_upload = ctx.job_log_upload.take();
  let flush = async {
    if let Some(handle) = live_log
      && tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .is_err()
    {
      tracing::warn!("live-log flush timed out; durable logs unaffected");
    }
  };
  let job_log = async {
    if let Some(handle) = job_log_upload
      && let Err(e) = handle.await
    {
      tracing::warn!(error = %e, "combined job-log upload task did not join cleanly");
    }
  };
  let del = delete_session(&ctx.client, &ctx.broker_url, &ctx.token, &ctx.session_id);
  let ((), (), delete_result) = tokio::join!(flush, job_log, del);
  if let Err(e) = delete_result {
    tracing::debug!(error = %e, "session delete failed (non-fatal)");
  }
}

#[cfg(test)]
#[path = "tests/helpers.rs"]
mod tests;
