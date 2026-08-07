//! GHES V1 protocol types: connection data, service definitions, timeline records.

use serde::{Deserialize, Serialize};

/// Service GUIDs for GHES V1 timeline API routing.
pub mod service_guids {
  /// The timeline service GUID.
  pub const TIMELINE: &str = "8893bc5b-35b2-4be7-83cb-99d683ff51a0";
  /// The log files service GUID.
  pub const LOG_FILES: &str = "46f5667d-263a-4684-91b1-dff7fdcf64e2";
  /// The log lines service GUID.
  pub const LOG_LINES: &str = "858983e4-19bd-4c5b-bfe2-f1ee9ef65722";
  /// The job-finish service GUID.
  pub const JOB_FINISH: &str = "557624af-b29e-4c20-8ab0-0399d2204f3f";
  /// The agent-delete service GUID.
  pub const AGENT_DELETE: &str = "e298ef32-5878-4cab-993c-043836571f42";
}

/// API version strings for different V1 endpoints.
pub mod api_versions {
  /// The default API version used for most V1 requests.
  pub const DEFAULT: &str = "5.1-preview";
  /// The API version for the agent-delete endpoint.
  pub const AGENT_DELETE: &str = "6.0-preview.2";
  /// The API version for the job-finish endpoint.
  pub const JOB_FINISH: &str = "2.0-preview.1";
}

/// Response from `_apis/connectionData`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionData {
  /// The `instanceId` of the GHES server instance.
  pub instance_id: String,
  /// The `locationServiceData` holding the available service definitions.
  pub location_service_data: LocationServiceData,
}

/// The `locationServiceData` section of `ConnectionData`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationServiceData {
  /// The `serviceDefinitions` — the list of resolvable V1 service endpoints.
  pub service_definitions: Vec<ServiceDefinition>,
}

/// A service endpoint definition from GHES connection data.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDefinition {
  /// The `identifier` GUID matching a `service_guids::*` constant.
  pub identifier: String,
  /// The `serviceType` label for this endpoint.
  pub service_type: Option<String>,
  /// The `displayName` label for this endpoint.
  pub display_name: Option<String>,
  /// The `relativePath` template used to build the endpoint URL.
  pub relative_path: Option<String>,
}

/// A timeline record representing a step/task in GHES V1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TimelineRecord {
  /// The record's `Id` (a GUID string).
  pub id: String,
  /// The `ParentId` of the enclosing record, if this is a nested step.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub parent_id: Option<String>,
  /// The `Type` of this record (e.g. `Job`, `Task`).
  #[serde(rename = "Type")]
  pub record_type: Option<String>,
  /// The `Name` shown in the GitHub UI for this record.
  pub name: Option<String>,
  /// The `State` (pending / in-progress / completed) of this record.
  pub state: Option<TimelineRecordState>,
  /// The `Result` of this record once completed.
  pub result: Option<TimelineRecordResult>,
  /// The `StartTime` of this record, ISO 8601.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub start_time: Option<String>,
  /// The `FinishTime` of this record, ISO 8601.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub finish_time: Option<String>,
  /// The `Log` reference for this record's uploaded log, if any.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub log: Option<LogReference>,
  /// The `Order` position of this record among its siblings.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub order: Option<i32>,
  /// The `ErrorCount` recorded for this record.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error_count: Option<i32>,
  /// The `WarningCount` recorded for this record.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub warning_count: Option<i32>,
}

/// The lifecycle state of a `TimelineRecord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineRecordState {
  /// Not yet started.
  Pending,
  /// Currently running.
  InProgress,
  /// Finished (see `TimelineRecordResult` for the outcome).
  Completed,
}

/// The outcome of a completed `TimelineRecord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineRecordResult {
  /// Completed without errors.
  Succeeded,
  /// Completed with warnings but no errors.
  SucceededWithIssues,
  /// Completed with an error.
  Failed,
  /// Cancelled before completion.
  Cancelled,
  /// Skipped (its condition was not met).
  Skipped,
  /// Abandoned (e.g. the job was torn down before it ran).
  Abandoned,
}

/// A reference to a record's uploaded log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LogReference {
  /// The log's numeric id.
  pub id: i64,
}

/// Job finish event for V1 API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct JobEvent {
  /// The event `Name` (e.g. `JobCompleted`).
  pub name: String,
  /// The `JobId` this event reports for.
  pub job_id: String,
  /// The `RequestId` of the job this event reports for.
  pub request_id: i64,
  /// The `Result` the job finished with.
  pub result: TimelineRecordResult,
  /// The job's `OutputVariables`, if it produced any.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub output_variables: Option<serde_json::Value>,
}
