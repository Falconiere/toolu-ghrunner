use serde::Serialize;

use shared::RunnerError;

// Re-export Twirp request/response types so callers don't need to change imports.
pub use super::results_types::{
  CreateJobLogsMetadataRequest, CreateStepLogsMetadataRequest, GetJobLogsSignedBlobUrlRequest,
  GetStepLogsSignedBlobUrlRequest, SignedBlobUrlResponse, StepUpdateEntry,
  WorkflowStepsUpdateRequest,
};

/// Twirp RPC service path for step status updates.
const WORKFLOW_STEP_UPDATE_SERVICE: &str =
  "twirp/github.actions.results.api.v1.WorkflowStepUpdateService/";

/// Twirp RPC service path for log/summary uploads.
const RESULTS_RECEIVER_SERVICE: &str = "twirp/results.services.receiver.Receiver/";

/// Azure blob storage type string (from Results Service protocol).
pub const BLOB_STORAGE_AZURE: &str = "BLOB_STORAGE_TYPE_AZURE";

/// Update workflow step statuses (start, complete, skip).
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn update_workflow_steps(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  request: &WorkflowStepsUpdateRequest,
) -> Result<(), RunnerError> {
  twirp_post(
    client,
    results_url,
    token,
    WORKFLOW_STEP_UPDATE_SERVICE,
    "WorkflowStepsUpdate",
    request,
  )
  .await?;
  Ok(())
}

/// Request a signed blob URL for uploading step logs.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP or parse failures.
pub async fn get_step_logs_signed_blob_url(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  request: &GetStepLogsSignedBlobUrlRequest,
) -> Result<SignedBlobUrlResponse, RunnerError> {
  let body = twirp_post(
    client,
    results_url,
    token,
    RESULTS_RECEIVER_SERVICE,
    "GetStepLogsSignedBlobURL",
    request,
  )
  .await?;

  serde_json::from_str(&body)
    .map_err(|e| RunnerError::Protocol(format!("step logs signed URL parse: {e}")))
}

/// Submit step logs metadata (marks upload complete).
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn create_step_logs_metadata(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  request: &CreateStepLogsMetadataRequest,
) -> Result<(), RunnerError> {
  twirp_post(
    client,
    results_url,
    token,
    RESULTS_RECEIVER_SERVICE,
    "CreateStepLogsMetadata",
    request,
  )
  .await?;
  Ok(())
}

/// Request a signed blob URL for uploading job-level logs.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP or parse failures.
pub async fn get_job_logs_signed_blob_url(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  request: &GetJobLogsSignedBlobUrlRequest,
) -> Result<SignedBlobUrlResponse, RunnerError> {
  let body = twirp_post(
    client,
    results_url,
    token,
    RESULTS_RECEIVER_SERVICE,
    "GetJobLogsSignedBlobURL",
    request,
  )
  .await?;

  serde_json::from_str(&body)
    .map_err(|e| RunnerError::Protocol(format!("job logs signed URL parse: {e}")))
}

/// Submit job logs metadata (marks upload complete).
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn create_job_logs_metadata(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  request: &CreateJobLogsMetadataRequest,
) -> Result<(), RunnerError> {
  twirp_post(
    client,
    results_url,
    token,
    RESULTS_RECEIVER_SERVICE,
    "CreateJobLogsMetadata",
    request,
  )
  .await?;
  Ok(())
}

/// Upload a log file to a SAS URL (Azure Blob Storage or compatible).
/// Uses BlockBlob (single-shot upload). Matches actions/runner `UploadBlockFileAsync`.
/// When `compressed` is true, sets `Content-Type: application/gzip` and
/// `Content-Encoding: gzip`; otherwise uses `Content-Type: text/plain`.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn upload_log_blob(
  client: &reqwest::Client,
  sas_url: &str,
  blob_storage_type: &str,
  content: Vec<u8>,
  compressed: bool,
) -> Result<(), RunnerError> {
  let content_type = if compressed {
    "application/gzip"
  } else {
    "text/plain"
  };
  let mut req = client
    .put(sas_url)
    .header(reqwest::header::CONTENT_TYPE, content_type);
  if compressed {
    req = req.header("Content-Encoding", "gzip");
  }
  if blob_storage_type == BLOB_STORAGE_AZURE {
    req = req.header("x-ms-blob-type", "BlockBlob");
  }
  let response = req
    .body(content)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("log blob upload failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let text = response.text().await.unwrap_or_default();
    return Err(RunnerError::Protocol(format!(
      "log blob upload status {status}: {text}"
    )));
  }
  Ok(())
}

async fn twirp_post<T: Serialize>(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  service_path: &str,
  method: &str,
  body: &T,
) -> Result<String, RunnerError> {
  let base = results_url.trim_end_matches('/');
  let url = format!("{base}/{service_path}{method}");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .header(reqwest::header::ACCEPT, "application/json")
    .json(body)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("twirp {method} failed: {e}")))?;

  let status = response.status();
  let text = response.text().await.unwrap_or_default();

  if !status.is_success() {
    return Err(RunnerError::Protocol(format!(
      "twirp {method} status {status}: {text}"
    )));
  }

  Ok(text)
}
