//! Twirp request/response types for the GitHub Actions Results Service.

use serde::Serialize;

use super::types::{Conclusion, Status};

/// A single step update entry for `WorkflowStepsUpdate`.
/// Uses snake_case JSON (Twirp/proto convention -- matches actions/runner C# SnakeCaseNamingStrategy).
#[derive(Debug, Clone, Serialize)]
pub struct StepUpdateEntry {
  pub external_id: String,
  pub number: u32,
  pub name: String,
  pub status: Status,
  pub conclusion: Option<Conclusion>,
  pub started_at: Option<String>,
  pub completed_at: Option<String>,
}

/// Request body for `WorkflowStepsUpdate` Twirp RPC.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowStepsUpdateRequest {
  pub steps: Vec<StepUpdateEntry>,
  pub change_order: u64,
  pub workflow_job_run_backend_id: String,
  pub workflow_run_backend_id: String,
}

/// Response from `GetStepLogsSignedBlobURL` / `GetJobLogsSignedBlobURL`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SignedBlobUrlResponse {
  pub logs_url: String,
  pub blob_storage_type: String,
}

/// Request for `GetStepLogsSignedBlobURL` (step-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct GetStepLogsSignedBlobUrlRequest {
  pub workflow_job_run_backend_id: String,
  pub workflow_run_backend_id: String,
  pub step_backend_id: String,
}

/// Request for `CreateStepLogsMetadata` (step-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct CreateStepLogsMetadataRequest {
  pub workflow_job_run_backend_id: String,
  pub workflow_run_backend_id: String,
  pub step_backend_id: String,
  pub uploaded_at: String,
  pub line_count: u64,
}

/// Request for `GetJobLogsSignedBlobURL` (job-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct GetJobLogsSignedBlobUrlRequest {
  pub workflow_job_run_backend_id: String,
  pub workflow_run_backend_id: String,
}

/// Request for `CreateJobLogsMetadata` (job-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct CreateJobLogsMetadataRequest {
  pub workflow_job_run_backend_id: String,
  pub workflow_run_backend_id: String,
  pub uploaded_at: String,
  pub line_count: u64,
}
