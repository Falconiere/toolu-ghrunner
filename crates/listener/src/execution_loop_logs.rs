//! Private log spool fanout and Results Service upload tasks.

use super::{ForwarderState, FwdConfig, LiveLogLine, mask_line, mpsc};
use crate::helpers::ResultsCtx;
use crate::log_uploader::StreamerConfig;

/// Spawn a per-step log streamer (only when a Results Service URL is
/// configured), register its line sender, and track the upload task so
/// its log URL can be backfilled on drain.
pub(super) fn spawn_step_uploader(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  step_id: &str,
  step_name: &str,
) {
  let Some(ref url) = cfg.results_url else {
    return;
  };
  let (tx, handle) = crate::log_uploader::spawn(StreamerConfig {
    client: cfg.results_client.clone(),
    results_url: url.clone(),
    token: cfg.results_token.clone(),
    run_backend_id: cfg.run_backend_id.clone(),
    job_backend_id: cfg.job_backend_id.clone(),
    step_backend_id: step_id.to_owned(),
    step_name: step_name.to_owned(),
    data_dir: cfg.data_dir.clone(),
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
pub(super) async fn forward_log_line(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
  step_id: &str,
  line: &str,
) {
  // The file sink's redactor runs on the same Mutex, so the
  // registration that put this secret into the masker is visible here.
  let masked = mask_line(&cfg.masker, line);
  if let Some(spool) = state.job_log.as_mut()
    && let Err(error) = spool.append(&masked).await
  {
    tracing::warn!(error = %error, "job log spool write failed");
    state.job_log = None;
  }
  if let Some(tx) = state.uploaders.get(step_id) {
    let _ = tx.send(masked.clone()).await;
  }
  forward_live_line(state, cfg, step_id, masked);
}

fn forward_live_line(state: &mut ForwarderState, cfg: &FwdConfig, step_id: &str, masked: String) {
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

/// Spawn the combined job-log upload without delaying `complete_job`.
/// Session cleanup joins this handle on success and error paths. The task
/// owns no listener sender, so it cannot keep the journal channel open.
pub(super) async fn spawn_job_log_upload(
  state: &mut ForwarderState,
  cfg: &FwdConfig,
) -> Option<tokio::task::JoinHandle<()>> {
  let spool = state.job_log.take()?;
  let results_url = cfg.results_url.clone()?;
  let (path, line_count) = match spool.finish().await {
    Ok(finished) => finished,
    Err(error) => {
      tracing::warn!(error = %error, "job log spool flush failed");
      return None;
    },
  };
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
    if let Some(count) = crate::log_uploader::upload_job_logs(&rctx, path, line_count).await {
      tracing::info!(line_count = count, "job log uploaded");
    }
  }))
}
