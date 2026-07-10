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
  pub message_type: String,
  pub plan: TaskOrchestrationPlanReference,
  #[serde(default)]
  pub timeline: Option<serde_json::Value>,
  pub job_id: String,
  pub job_display_name: String,
  pub job_name: String,
  #[serde(default)]
  pub request_id: i64,
  #[serde(default)]
  pub locked_until: Option<String>,
  #[serde(default)]
  pub steps: Vec<ActionStep>,
  #[serde(default)]
  pub variables: HashMap<String, VariableValue>,
  #[serde(default)]
  pub mask: Vec<MaskHint>,
  #[serde(default)]
  pub resources: JobResources,
  #[serde(default, rename = "runServiceUrl")]
  pub run_service_url_field: Option<String>,
  #[serde(default)]
  pub context_data: HashMap<String, PipelineContextData>,
  #[serde(default)]
  pub workspace: Option<WorkspaceOptions>,
  #[serde(default, rename = "environmentVariables")]
  pub environment_variables: Vec<serde_json::Value>,
  #[serde(default)]
  pub defaults: Vec<serde_json::Value>,
  #[serde(default, rename = "fileTable")]
  pub file_table: Vec<serde_json::Value>,
}

impl AgentJobRequestMessage {
  /// Get the run_service_url. Present for V2 (GitHub.com), absent for V1 (GHES).
  pub fn run_service_url(&self) -> Option<&String> {
    self
      .run_service_url_field
      .as_ref()
      .filter(|s| !s.is_empty())
  }

  /// Get the GHES server URL from SystemVssConnection endpoint.
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

  /// Get the live log WebSocket URL from SystemVssConnection endpoint data.
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
  #[serde(default)]
  pub scope_identifier: Option<String>,
  pub plan_id: String,
  #[serde(default)]
  pub plan_type: Option<String>,
  #[serde(default)]
  pub version: Option<i64>,
}
