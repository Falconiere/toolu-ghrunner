use serde::{Deserialize, Serialize};

use super::types::{Annotation, Conclusion};
use shared::RunnerError;

/// Request body for `POST {run_service_url}/acquirejob`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireJobRequest {
  pub job_message_id: String,
  #[serde(rename = "runnerOS")]
  pub runner_os: String,
  pub billing_owner_id: String,
}

/// Response from acquirejob — wraps the job body + plan ID from header.
#[derive(Debug, Clone)]
pub struct AcquireJobResponse {
  pub plan_id: String,
  pub body: serde_json::Value,
  /// Bearer token for subsequent run service calls (from x-actions-results-token).
  pub run_service_token: Option<String>,
}

/// Request body for `POST {run_service_url}/renewjob`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewJobRequest {
  pub plan_id: String,
  pub job_id: String,
}

/// Response from renewjob.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewJobResponse {
  pub locked_until: String,
}

/// Request body for `POST {run_service_url}/completejob`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteJobRequest {
  pub plan_id: String,
  pub job_id: String,
  pub request_id: i64,
  pub conclusion: Conclusion,
  pub outputs: serde_json::Value,
  pub step_results: Vec<super::types::StepResult>,
  pub annotations: Vec<Annotation>,
}

/// Acquire a job from the run service.
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
    return Err(RunnerError::Protocol(format!(
      "acquire job status {status}: {body}"
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

/// Renew a job lock. Call every 60 seconds.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
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
    return Err(RunnerError::Protocol(format!(
      "renew job status {status}: {body}"
    )));
  }

  response
    .json::<RenewJobResponse>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("renew job parse: {e}")))
}

/// Complete a job with final conclusion and step results.
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
    return Err(RunnerError::Protocol(format!(
      "complete job status {status}: {body}"
    )));
  }

  Ok(())
}
