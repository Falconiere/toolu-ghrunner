use shared::RunnerError;

// Re-export Twirp request/response types so callers don't need to change imports.
pub use super::results_types::{
  CreateJobLogsMetadataRequest, CreateStepLogsMetadataRequest, GetJobLogsSignedBlobUrlRequest,
  GetStepLogsSignedBlobUrlRequest, SignedBlobUrlResponse, StepUpdateEntry,
  WorkflowStepsUpdateRequest,
};

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
  crate::net::update_workflow_steps(client, results_url, token, request).await
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
  crate::net::get_step_logs_signed_blob_url(client, results_url, token, request).await
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
  crate::net::create_step_logs_metadata(client, results_url, token, request).await
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
  crate::net::get_job_logs_signed_blob_url(client, results_url, token, request).await
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
  crate::net::create_job_logs_metadata(client, results_url, token, request).await
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
  crate::net::upload_log_blob(client, sas_url, blob_storage_type, content, compressed).await
}
