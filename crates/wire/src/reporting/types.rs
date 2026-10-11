use serde::Serialize;
use serde_repr::{Deserialize_repr, Serialize_repr};

/// Twirp step status values (matches GitHub protocol).
///
/// Values: 0=Unknown, 3=InProgress, 5=Pending, 6=Completed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Status {
  /// Status has not been set.
  Unknown = 0,
  /// The step is currently running.
  InProgress = 3,
  /// The step has not started yet.
  Pending = 5,
  /// The step has finished (see the paired `Conclusion` for the outcome).
  Completed = 6,
}

/// Twirp step conclusion values (matches GitHub protocol).
///
/// Values: 0=Unknown, 2=Success, 3=Failure, 4=Cancelled, 7=Skipped
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Conclusion {
  /// Conclusion has not been set.
  Unknown = 0,
  /// The step/job completed successfully.
  Success = 2,
  /// The step/job failed.
  Failure = 3,
  /// The step/job was cancelled.
  Cancelled = 4,
  /// The step/job was skipped.
  Skipped = 7,
}

/// Run Service step record state, serialized as upstream's camelCase
/// `TimelineRecordState` name. Every step result is sent once completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
  /// The step has finished.
  Completed,
}

/// Result for a single step, sent in completejob.
///
/// Mirrors upstream `StepResult.cs`: explicit `snake_case` member names
/// (Newtonsoft's camel-casing leaves them unchanged) and string enums.
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
  /// Backend UUID of the step this result is for (never the context name).
  pub external_id: String,
  /// 1-based step order within the job.
  pub number: u32,
  /// Display name of the step.
  pub name: String,
  /// Action identity (`owner/repo[/path]`, local path, image, shell or runner step).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub action_name: Option<String>,
  /// Git ref of a remote repository action.
  #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
  pub git_ref: Option<String>,
  /// Handler kind (`run`, `node24`, `composite`, `Dockerfile`, `DockerHub`, `runner`).
  #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
  pub kind: Option<String>,
  /// Final record state of the step.
  pub status: StepState,
  /// Final conclusion of the step as an upstream `TaskResult` name.
  pub conclusion: super::run_service::JobConclusion,
  /// ISO 8601 timestamp the step started.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub started_at: Option<String>,
  /// ISO 8601 timestamp the step completed.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub completed_at: Option<String>,
  /// Blob URL from `GetStepLogsSignedBlobURL` — links uploaded logs to this step.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub completed_log_url: Option<String>,
  /// Number of lines in the step's uploaded log.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub completed_log_lines: Option<u64>,
  /// Annotations attributed to this step; upstream always sends the list.
  pub annotations: Vec<Annotation>,
}

/// Run Service annotation severity, encoded as the official numeric enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum AnnotationLevel {
  /// No severity was supplied.
  Unknown = 0,
  /// Informational annotation.
  Notice = 1,
  /// Warning annotation.
  Warning = 2,
  /// Error annotation (`::error::` maps to `FAILURE`).
  Failure = 3,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
  value == &T::default()
}

/// Annotation attached to a step result or to the completed job.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
  /// Numeric Run Service severity (`NOTICE=1`, `WARNING=2`, `FAILURE=3`).
  pub level: AnnotationLevel,
  /// The annotation text.
  pub message: String,
  /// Optional short title shown with the annotation.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub title: Option<String>,
  /// Optional unformatted details from a runner-origin annotation.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub raw_details: Option<String>,
  /// Repository-relative source path, if present.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  /// Whether the runner itself caused this issue.
  #[serde(skip_serializing_if = "is_default")]
  pub is_infrastructure_issue: bool,
  /// First source line, omitted when absent or zero.
  #[serde(skip_serializing_if = "is_default")]
  pub start_line: i64,
  /// Last source line, omitted when absent or zero.
  #[serde(skip_serializing_if = "is_default")]
  pub end_line: i64,
  /// First source column, omitted when absent or zero.
  #[serde(skip_serializing_if = "is_default")]
  pub start_column: i64,
  /// Last source column, omitted when absent or zero.
  #[serde(skip_serializing_if = "is_default")]
  pub end_column: i64,
  /// Number of the reporting step, omitted for job-level issues.
  #[serde(skip_serializing_if = "is_default")]
  pub step_number: i64,
}
