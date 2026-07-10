//! Async transport for the Actions Run Service: acquire / renew / complete job.
//!
//! Request and response types live in [`crate::reporting::run_service`]
//! alongside the higher-level reporting wrappers. This file only owns
//! the HTTP.

use shared::RunnerError;

use crate::reporting::run_service::{
  AcquireJobRequest, AcquireJobResponse, CompleteJobRequest, RenewJobRequest, RenewJobResponse,
};

/// Acquire a job from the run service.
///
/// Reads `x-plan-id` and `x-actions-results-token` headers off the
/// response, parses the body into a JSON value, and packages them
/// into an [`AcquireJobResponse`].
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP or parse failures.
pub async fn acquire_job(
  client: &reqwest::Client,
  run_service_url: &str,
  token: &str,
  request: &AcquireJobRequest,
) -> Result<AcquireJobResponse, RunnerError> {
  let url = format!("{run_service_url}/acquirejob");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("acquire job failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body = %body, "acquire job failed");
    return Err(RunnerError::Protocol(format!(
      "acquire job status {status}: see debug log"
    )));
  }

  let plan_id = response
    .headers()
    .get("x-plan-id")
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
    .to_owned();

  let run_service_token = response
    .headers()
    .get("x-actions-results-token")
    .and_then(|v| v.to_str().ok())
    .map(ToOwned::to_owned);

  let body = response
    .json::<serde_json::Value>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("acquire job parse: {e}")))?;

  Ok(AcquireJobResponse {
    plan_id,
    body,
    run_service_token,
  })
}

/// Renew a job lock. Called every ~60 seconds during job execution.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP or parse failures.
pub async fn renew_job(
  client: &reqwest::Client,
  run_service_url: &str,
  token: &str,
  request: &RenewJobRequest,
) -> Result<RenewJobResponse, RunnerError> {
  let url = format!("{run_service_url}/renewjob");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("renew job failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body = %body, "renew job failed");
    return Err(RunnerError::Protocol(format!(
      "renew job status {status}: see debug log"
    )));
  }

  response
    .json::<RenewJobResponse>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("renew job parse: {e}")))
}

/// Complete a job with its final conclusion, step results, and outputs.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn complete_job(
  client: &reqwest::Client,
  run_service_url: &str,
  token: &str,
  request: &CompleteJobRequest,
) -> Result<(), RunnerError> {
  let url = format!("{run_service_url}/completejob");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("complete job failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body = %body, "complete job failed");
    return Err(RunnerError::Protocol(format!(
      "complete job status {status}: see debug log"
    )));
  }

  Ok(())
}
