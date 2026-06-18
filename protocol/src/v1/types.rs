use serde::{Deserialize, Serialize};

/// Service GUIDs for GHES V1 timeline API routing.
pub mod service_guids {
  pub const TIMELINE: &str = "8893bc5b-35b2-4be7-83cb-99d683ff51a0";
  pub const LOG_FILES: &str = "46f5667d-263a-4684-91b1-dff7fdcf64e2";
  pub const LOG_LINES: &str = "858983e4-19bd-4c5b-bfe2-f1ee9ef65722";
  pub const JOB_FINISH: &str = "557624af-b29e-4c20-8ab0-0399d2204f3f";
  pub const AGENT_DELETE: &str = "e298ef32-5878-4cab-993c-043836571f42";
}

/// API version strings for different V1 endpoints.
pub mod api_versions {
  pub const DEFAULT: &str = "5.1-preview";
  pub const AGENT_DELETE: &str = "6.0-preview.2";
  pub const JOB_FINISH: &str = "2.0-preview.1";
}

/// Response from `_apis/connectionData`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionData {
  pub instance_id: String,
  pub location_service_data: LocationServiceData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationServiceData {
  pub service_definitions: Vec<ServiceDefinition>,
}

/// A service endpoint definition from GHES connection data.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDefinition {
  pub identifier: String,
  pub service_type: Option<String>,
  pub display_name: Option<String>,
  pub relative_path: Option<String>,
}

/// A timeline record representing a step/task in GHES V1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TimelineRecord {
  pub id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub parent_id: Option<String>,
  #[serde(rename = "Type")]
  pub record_type: Option<String>,
  pub name: Option<String>,
  pub state: Option<TimelineRecordState>,
  pub result: Option<TimelineRecordResult>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub start_time: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub finish_time: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub log: Option<LogReference>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub order: Option<i32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error_count: Option<i32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub warning_count: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineRecordState {
  Pending,
  InProgress,
  Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineRecordResult {
  Succeeded,
  SucceededWithIssues,
  Failed,
  Cancelled,
  Skipped,
  Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LogReference {
  pub id: i64,
}

/// Job finish event for V1 API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct JobEvent {
  pub name: String,
  pub job_id: String,
  pub request_id: i64,
  pub result: TimelineRecordResult,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub output_variables: Option<serde_json::Value>,
}
