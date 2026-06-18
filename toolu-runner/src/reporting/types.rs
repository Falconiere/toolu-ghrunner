use serde::Serialize;
use serde_repr::{Deserialize_repr, Serialize_repr};

/// Twirp step status values (matches GitHub protocol).
///
/// Values: 0=Unknown, 3=InProgress, 5=Pending, 6=Completed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Status {
  Unknown = 0,
  InProgress = 3,
  Pending = 5,
  Completed = 6,
}

/// Twirp step conclusion values (matches GitHub protocol).
///
/// Values: 0=Unknown, 2=Success, 3=Failure, 4=Cancelled, 7=Skipped
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Conclusion {
  Unknown = 0,
  Success = 2,
  Failure = 3,
  Cancelled = 4,
  Skipped = 7,
}

/// Result for a single step, sent in completejob.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
  pub external_id: String,
  pub number: u32,
  pub name: String,
  pub status: Status,
  pub conclusion: Conclusion,
  pub outcome: Conclusion,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub started_at: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub completed_at: Option<String>,
  /// Blob URL from GetStepLogsSignedBlobURL — links uploaded logs to this step.
  /// C# field name is `CompletedLogURL` (capital URL), so override camelCase.
  #[serde(rename = "completedLogURL", skip_serializing_if = "Option::is_none")]
  pub completed_log_url: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub completed_log_lines: Option<u64>,
}

/// Annotation attached to a step result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
  pub annotation_type: String,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub file: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub line: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub col: Option<u32>,
}
