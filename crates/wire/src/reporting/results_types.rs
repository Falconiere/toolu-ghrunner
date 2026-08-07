//! Twirp request/response types for the GitHub Actions Results Service.

use serde::Serialize;

use super::types::{Conclusion, Status};

/// A single step update entry for `WorkflowStepsUpdate`.
/// Uses `snake_case` JSON (Twirp/proto convention -- matches actions/runner C# `SnakeCaseNamingStrategy`).
#[derive(Debug, Clone, Serialize)]
pub struct StepUpdateEntry {
  /// Backend id of the step this update targets.
  pub external_id: String,
  /// 1-based step index within the job.
  pub number: u32,
  /// Display name of the step.
  pub name: String,
  /// Current step status (pending / in progress / completed).
  pub status: Status,
  /// Step conclusion once completed, if known yet.
  pub conclusion: Option<Conclusion>,
  /// ISO 8601 timestamp the step started, if known yet.
  pub started_at: Option<String>,
  /// ISO 8601 timestamp the step completed, if known yet.
  pub completed_at: Option<String>,
}

/// Request body for `WorkflowStepsUpdate` Twirp RPC.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowStepsUpdateRequest {
  /// The step updates in this batch.
  pub steps: Vec<StepUpdateEntry>,
  /// Monotonic sequence number ordering this update against others for the job.
  pub change_order: u64,
  /// Backend id of the job run these steps belong to.
  pub workflow_job_run_backend_id: String,
  /// Backend id of the workflow run these steps belong to.
  pub workflow_run_backend_id: String,
}

/// Response from `GetStepLogsSignedBlobURL` / `GetJobLogsSignedBlobURL`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SignedBlobUrlResponse {
  /// Signed URL to `PUT` the log content to.
  pub logs_url: String,
  /// The blob storage backend type the signed URL targets.
  pub blob_storage_type: String,
}

/// Request for `GetStepLogsSignedBlobURL` (step-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct GetStepLogsSignedBlobUrlRequest {
  /// Backend id of the job run the step belongs to.
  pub workflow_job_run_backend_id: String,
  /// Backend id of the workflow run the step belongs to.
  pub workflow_run_backend_id: String,
  /// Backend id of the step to get a signed log upload URL for.
  pub step_backend_id: String,
}

/// Request for `CreateStepLogsMetadata` (step-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct CreateStepLogsMetadataRequest {
  /// Backend id of the job run the step belongs to.
  pub workflow_job_run_backend_id: String,
  /// Backend id of the workflow run the step belongs to.
  pub workflow_run_backend_id: String,
  /// Backend id of the step the log metadata is for.
  pub step_backend_id: String,
  /// ISO 8601 timestamp the log content was uploaded.
  pub uploaded_at: String,
  /// Number of lines in the uploaded log content.
  pub line_count: u64,
}

/// Request for `GetJobLogsSignedBlobURL` (job-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct GetJobLogsSignedBlobUrlRequest {
  /// Backend id of the job run to get a signed log upload URL for.
  pub workflow_job_run_backend_id: String,
  /// Backend id of the workflow run the job belongs to.
  pub workflow_run_backend_id: String,
}

/// Request for `CreateJobLogsMetadata` (job-level logs).
#[derive(Debug, Clone, Serialize)]
pub struct CreateJobLogsMetadataRequest {
  /// Backend id of the job run the log metadata is for.
  pub workflow_job_run_backend_id: String,
  /// Backend id of the workflow run the job belongs to.
  pub workflow_run_backend_id: String,
  /// ISO 8601 timestamp the log content was uploaded.
  pub uploaded_at: String,
  /// Number of lines in the uploaded log content.
  pub line_count: u64,
}
