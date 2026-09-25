use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::types::Annotation;
use shared::RunnerError;

/// Request body for `POST {run_service_url}/acquirejob`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireJobRequest {
  /// Id of the broker message carrying the job to acquire.
  pub job_message_id: String,
  /// This host's OS label, sent as the `runnerOS` field.
  #[serde(rename = "runnerOS")]
  pub runner_os: String,
  /// Id of the billing owner the job runs under.
  pub billing_owner_id: String,
}

/// Response from acquirejob — wraps the job body + plan ID from header.
#[derive(Debug, Clone)]
pub struct AcquireJobResponse {
  /// Id of the orchestration plan the acquired job belongs to.
  pub plan_id: String,
  /// The raw acquired job body (broker's `AgentJobRequestMessage` JSON).
  pub body: serde_json::Value,
  /// Bearer token for subsequent run service calls (from x-actions-results-token).
  pub run_service_token: Option<String>,
}

/// Request body for `POST {run_service_url}/renewjob`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewJobRequest {
  /// Id of the orchestration plan the job belongs to.
  pub plan_id: String,
  /// Id of the job whose lock is being renewed.
  pub job_id: String,
}

/// Response from renewjob.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewJobResponse {
  /// ISO 8601 timestamp the lock is now held until.
  pub locked_until: String,
}

/// Request body for `POST {run_service_url}/completejob`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteJobRequest {
  /// Id of the orchestration plan the job belongs to.
  pub plan_id: String,
  /// Id of the job being completed.
  pub job_id: String,
  /// Id of the broker message that delivered the job, echoed back on completion.
  pub request_id: i64,
  /// Final job conclusion in the Run Service `TaskResult` wire format.
  pub conclusion: JobConclusion,
  /// Job-level outputs to report back.
  pub outputs: HashMap<String, CompleteJobOutput>,
  /// Per-step results for the completed job.
  pub step_results: Vec<super::types::StepResult>,
  /// Job-level annotations (errors/warnings/notices) to report back.
  pub annotations: Vec<Annotation>,
}

/// Run Service job result, serialized as the upstream `TaskResult` name.
/// The Results Service step conclusion uses a different numeric enum.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobConclusion {
  /// The job succeeded.
  Succeeded,
  /// The job failed.
  Failed,
  /// The job was canceled.
  Canceled,
  /// The job was skipped.
  Skipped,
}

/// One job-level output in the Run Service completion payload.
#[derive(Debug, Clone, Serialize)]
pub struct CompleteJobOutput {
  /// The resolved, non-secret output value.
  pub value: String,
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
