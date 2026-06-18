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
  crate::net::acquire_job(client, run_service_url, token, request).await
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
  crate::net::renew_job(client, run_service_url, token, request).await
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
  crate::net::complete_job(client, run_service_url, token, request).await
}
