use std::time::Duration;

use super::SessionCtx;
use super::execution_loop::execute_with_renewal;
use super::helpers::map_conclusion;
use crate::net::{PollParams, acknowledge_message, poll_message};
use crate::reporting::StepResult;
use crate::reporting::live_log::LiveLogStreamer;
use crate::reporting::run_service::{
  AcquireJobRequest, CompleteJobRequest, acquire_job, complete_job,
};
use protocol::messages::{BrokerMessage, BrokerMigrationBody, MessageType};
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerError};

/// Starting backoff for a network error during the poll loop.
const POLL_BACKOFF_START: Duration = Duration::from_secs(1);
/// Cap on the exponential backoff between poll retries.
const POLL_BACKOFF_MAX: Duration = Duration::from_secs(60);

pub(super) async fn poll_and_execute(ctx: &mut SessionCtx) -> Result<(), RunnerError> {
  let Some(msg) = poll_until_job(ctx).await? else {
    return Ok(());
  };

  let body = parse_job_request_body(&msg)?;

  let acquire_req = AcquireJobRequest {
    job_message_id: body.runner_request_id.clone(),
    runner_os: std::env::consts::OS.to_owned(),
    billing_owner_id: body.billing_owner_id.clone(),
  };
  let acquired = acquire_job(&ctx.client, &body.run_service_url, &ctx.token, &acquire_req).await?;
  let rs_token = acquired
    .run_service_token
    .clone()
    .unwrap_or_else(|| ctx.token.clone());
  tracing::info!(plan_id = %acquired.plan_id, "acquired job");
  let plan_id = acquired.plan_id.clone();

  let (conclusion, job_id, request_id, step_results, job_token) =
    run_acquired_job(ctx, &body.run_service_url, &rs_token, &acquired).await;
  let rs_token = job_token.unwrap_or(rs_token);

  // Acknowledge the broker message — required by the JIT protocol before complete_job.
  acknowledge_message(&ctx.client, &ctx.broker_url, &ctx.token, msg.message_id).await?;

  let complete_req = CompleteJobRequest {
    plan_id,
    job_id,
    request_id,
    conclusion: map_conclusion(conclusion),
    outputs: serde_json::Value::Object(serde_json::Map::new()),
    step_results,
    annotations: Vec::new(),
  };
  complete_job(&ctx.client, &body.run_service_url, &rs_token, &complete_req).await
}

/// Parse and execute the acquired job. Always returns a conclusion — never leaves
/// the job hanging on GitHub even if parsing fails.
/// Returns `(conclusion, job_id, request_id, step_results, optional_token)`.
async fn run_acquired_job(
  ctx: &mut SessionCtx,
  run_service_url: &str,
  rs_token: &str,
  acquired: &crate::reporting::run_service::AcquireJobResponse,
) -> (Conclusion, String, i64, Vec<StepResult>, Option<String>) {
  let job_msg: AgentJobRequestMessage = match serde_json::from_value(acquired.body.clone()) {
    Ok(msg) => msg,
    Err(e) => {
      tracing::error!(error = %e, "job message parse failed — completing with failure");
      return (
        Conclusion::Failure,
        "unknown".to_owned(),
        0,
        Vec::new(),
        None,
      );
    },
  };

  let job_token = extract_system_token(&job_msg);

  let _ = ctx
    .tx
    .send(ListenerEvent::JobAcquired {
      job_id: job_msg.job_id.clone(),
      run_service_url: run_service_url.to_owned(),
    })
    .await;

  let request_id = job_msg.request_id;
  let effective_token = job_token.as_deref().unwrap_or(rs_token);

  // Connect live log WebSocket for real-time log streaming to GitHub UI.
  let live_log_tx = connect_live_log(&job_msg, effective_token).await;

  let (conclusion, step_results) = execute_with_renewal(
    ctx,
    run_service_url,
    effective_token,
    &acquired.plan_id,
    &job_msg,
    live_log_tx,
  )
  .await;
  (
    conclusion,
    job_msg.job_id,
    request_id,
    step_results,
    job_token,
  )
}

async fn poll_until_job(ctx: &mut SessionCtx) -> Result<Option<BrokerMessage>, RunnerError> {
  let mut backoff = POLL_BACKOFF_START;
  loop {
    let result = poll_once(ctx).await;
    match result {
      PollOutcome::Cancelled => return Ok(None),
      PollOutcome::NoWork => {
        backoff = POLL_BACKOFF_START;
        continue;
      },
      PollOutcome::Migrated(new_url) => {
        ctx.broker_url = new_url;
        backoff = POLL_BACKOFF_START;
        continue;
      },
      PollOutcome::Job(msg) => return Ok(Some(msg)),
      PollOutcome::NetworkError(e) => {
        let backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
        tracing::warn!(
          error = %e,
          backoff_ms,
          "poll failed — retrying after backoff"
        );
        if sleep_or_cancel(ctx, backoff).await {
          return Ok(None);
        }
        backoff = backoff.saturating_mul(2).min(POLL_BACKOFF_MAX);
      },
    }
  }
}

/// Outcome of a single `poll_message` call, classified for the loop.
enum PollOutcome {
  /// Long-poll returned 202 — broker accepted the connection but had
  /// no work.
  NoWork,
  /// Long-poll returned a `BrokerMigration` message; carries the new
  /// broker URL.
  Migrated(String),
  /// Long-poll returned a `RunnerJobRequest` — caller should acquire.
  Job(BrokerMessage),
  /// Network/HTTP failure — caller should back off and retry.
  NetworkError(RunnerError),
  /// Cancellation token tripped during the poll — caller should exit.
  Cancelled,
}

async fn poll_once(ctx: &SessionCtx) -> PollOutcome {
  let params = PollParams {
    client: &ctx.client,
    server_url_v2: &ctx.broker_url,
    token: &ctx.token,
    session_id: &ctx.session_id,
    runner_version: "3.0.0",
    os: std::env::consts::OS,
    architecture: std::env::consts::ARCH,
  };

  let poll_fut = poll_message(&params);
  tokio::select! {
    biased;
    () = ctx.cancel.cancelled() => PollOutcome::Cancelled,
    result = poll_fut => match result {
      Ok(None) => PollOutcome::NoWork,
      Ok(Some(msg)) => match msg.message_type {
        MessageType::BrokerMigration => match parse_migration(&msg.body) {
          Ok(new_url) => PollOutcome::Migrated(new_url),
          Err(e) => PollOutcome::NetworkError(e),
        },
        MessageType::RunnerJobRequest => PollOutcome::Job(msg),
      },
      Err(e) => PollOutcome::NetworkError(e),
    },
  }
}

fn parse_migration(body: &str) -> Result<String, RunnerError> {
  let migration: BrokerMigrationBody = serde_json::from_str(body)
    .map_err(|e| RunnerError::Protocol(format!("migration parse: {e}")))?;
  tracing::info!(new_url = %migration.broker_base_url, "broker migration");
  Ok(migration.broker_base_url)
}

/// Sleep for `duration`, returning `true` if cancellation fired first.
async fn sleep_or_cancel(ctx: &SessionCtx, duration: Duration) -> bool {
  tokio::select! {
    biased;
    () = ctx.cancel.cancelled() => true,
    () = tokio::time::sleep(duration) => false,
  }
}

fn parse_job_request_body(
  msg: &BrokerMessage,
) -> Result<protocol::messages::RunnerJobRequestBody, RunnerError> {
  serde_json::from_str(&msg.body)
    .map_err(|e| RunnerError::Protocol(format!("job request body parse: {e}")))
}

/// Connect live log WebSocket for real-time log streaming to GitHub UI.
/// Returns None on failure — live logs are best-effort.
async fn connect_live_log(
  job_msg: &AgentJobRequestMessage,
  fallback_token: &str,
) -> Option<tokio::sync::mpsc::Sender<crate::reporting::live_log::LiveLogLine>> {
  let url = job_msg.feed_stream_url()?;
  let token = super::helpers::system_vss_access_token(job_msg);
  let ws_token = token.as_deref().unwrap_or(fallback_token);
  let (tx, handle) = LiveLogStreamer::connect(&url, ws_token).await?;
  // Detach — handle completes when tx is dropped.
  tokio::spawn(async move {
    if let Err(e) = handle.await {
      tracing::warn!(error = %e, "live log WebSocket task panicked");
    }
  });
  Some(tx)
}

/// Extract the SystemVssConnection AccessToken from job message endpoints.
fn extract_system_token(job_msg: &AgentJobRequestMessage) -> Option<String> {
  let token = super::helpers::system_vss_access_token(job_msg);
  if token.is_none() {
    let endpoint = job_msg
      .resources
      .endpoints
      .iter()
      .find(|e| e.name.eq_ignore_ascii_case("SystemVssConnection"));
    let auth_keys: Vec<&str> = endpoint
      .and_then(|e| e.authorization.as_ref())
      .map(|a| a.parameters.keys().map(String::as_str).collect())
      .unwrap_or_default();
    tracing::warn!(
      endpoint_found = endpoint.is_some(),
      auth_scheme = endpoint
        .and_then(|e| e.authorization.as_ref())
        .map(|a| a.scheme.as_str())
        .unwrap_or("<none>"),
      auth_keys = ?auth_keys,
      "SystemVssConnection AccessToken not found"
    );
  }
  token
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Verifies the backoff schedule required by the failure-mode spec:
  /// doubles on each failure and caps at 60s.
  #[test]
  fn poll_backoff_doubles_and_caps_at_60s() {
    let mut backoff = POLL_BACKOFF_START;
    // Doubling sequence: 1, 2, 4, 8, 16, 32, 60, 60, 60…
    let expected = [
      Duration::from_secs(1),
      Duration::from_secs(2),
      Duration::from_secs(4),
      Duration::from_secs(8),
      Duration::from_secs(16),
      Duration::from_secs(32),
      Duration::from_secs(60),
      Duration::from_secs(60),
      Duration::from_secs(60),
    ];
    for want in expected {
      assert_eq!(backoff, want, "backoff schedule drift");
      backoff = backoff.saturating_mul(2).min(POLL_BACKOFF_MAX);
    }
  }
}
