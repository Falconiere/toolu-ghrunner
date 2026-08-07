//! Top-level job request message and plan reference.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::context_data::PipelineContextData;
use super::resource::{JobResources, MaskHint, VariableValue, WorkspaceOptions};
use super::step::ActionStep;

/// The full job request message received from GitHub after `acquirejob`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentJobRequestMessage {
  /// Wire discriminator; always `"JobRequest"` for this message shape.
  pub message_type: String,
  /// Reference to the orchestration plan this job belongs to.
  pub plan: TaskOrchestrationPlanReference,
  /// Opaque timeline reference; `timeline_id()` extracts its `id` field.
  #[serde(default)]
  pub timeline: Option<serde_json::Value>,
  /// The GitHub Actions job id.
  pub job_id: String,
  /// The job name as displayed in the GitHub UI.
  pub job_display_name: String,
  /// The job's internal name (workflow YAML key).
  pub job_name: String,
  /// Monotonic request counter for this job's lock/renewal exchanges.
  #[serde(default)]
  pub request_id: i64,
  /// Timestamp until which the job lock is currently held, if known.
  #[serde(default)]
  pub locked_until: Option<String>,
  /// The job's ordered list of steps (`uses:` / `run:` entries).
  #[serde(default)]
  pub steps: Vec<ActionStep>,
  /// Job-level `env:`/variable values, keyed by variable name.
  #[serde(default)]
  pub variables: HashMap<String, VariableValue>,
  /// Values GitHub asks the runner to mask from logs.
  #[serde(default)]
  pub mask: Vec<MaskHint>,
  /// Endpoints, repositories, and other resources available to the job.
  #[serde(default)]
  pub resources: JobResources,
  /// Wire field `runServiceUrl`; present on V2 (github.com), absent on V1 (GHES).
  #[serde(default, rename = "runServiceUrl")]
  pub run_service_url_field: Option<String>,
  /// The `${{ }}` pipeline context values (e.g. `github`, `env`), keyed by context name.
  #[serde(default)]
  pub context_data: HashMap<String, PipelineContextData>,
  /// Workspace layout options for this job, if specified.
  #[serde(default)]
  pub workspace: Option<WorkspaceOptions>,
  /// Wire field `environmentVariables`; raw (unparsed) job environment entries.
  #[serde(default, rename = "environmentVariables")]
  pub environment_variables: Vec<serde_json::Value>,
  /// Job-level `defaults:` entries (e.g. `defaults.run`), unparsed.
  #[serde(default)]
  pub defaults: Vec<serde_json::Value>,
  /// Wire field `fileTable`; index-to-path mapping used by step file references.
  #[serde(default, rename = "fileTable")]
  pub file_table: Vec<serde_json::Value>,
}

impl AgentJobRequestMessage {
  /// Get the `run_service_url`. Present for V2 (`GitHub.com`), absent for V1 (GHES).
  pub fn run_service_url(&self) -> Option<&String> {
    self
      .run_service_url_field
      .as_ref()
      .filter(|s| !s.is_empty())
  }

  /// Get the GHES server URL from the `SystemVssConnection` endpoint.
  pub fn server_url(&self) -> Option<&str> {
    self
      .resources
      .endpoints
      .iter()
      .find(|e| e.name == "SystemVssConnection")
      .and_then(|e| e.url.as_deref())
  }

  /// Get the timeline ID from the job message's timeline reference.
  pub fn timeline_id(&self) -> Option<&str> {
    self
      .timeline
      .as_ref()
      .and_then(|t| t.get("id"))
      .and_then(|v| v.as_str())
  }

  /// Get the live log WebSocket URL from `SystemVssConnection` endpoint data.
  /// C# runner reads `FeedStreamUrl` and converts https→wss for WebSocket.
  pub fn feed_stream_url(&self) -> Option<String> {
    self
      .resources
      .endpoints
      .iter()
      .find(|e| e.name == "SystemVssConnection")
      .and_then(|e| e.data.get("FeedStreamUrl"))
      .filter(|url| !url.is_empty())
      .map(|url| {
        url
          .replace("https://", "wss://")
          .replace("http://", "ws://")
      })
  }
}

/// Reference to the task orchestration plan for this job.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOrchestrationPlanReference {
  /// The scope (e.g. project id) the plan belongs to, if reported.
  #[serde(default)]
  pub scope_identifier: Option<String>,
  /// The orchestration plan's id.
  pub plan_id: String,
  /// The plan's type, if reported.
  #[serde(default)]
  pub plan_type: Option<String>,
  /// The plan's version number, if reported.
  #[serde(default)]
  pub version: Option<i64>,
}
