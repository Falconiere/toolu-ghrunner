//! Job resource types: variables, masking, endpoints, authorization, workspace.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A variable in the job message (may be secret).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableValue {
  /// The variable's value.
  pub value: String,
  /// Whether this value should be treated as a secret (masked in logs).
  #[serde(default)]
  pub is_secret: bool,
}

/// Hint for values that should be masked in logs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskHint {
  /// The literal value to mask.
  pub value: String,
  /// Wire field `type`; the kind of mask hint (e.g. `"regex"`), if reported.
  #[serde(default, rename = "type")]
  pub mask_type: Option<String>,
}

/// Resources available to the job (endpoints, authorizations).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResources {
  /// The job's service endpoints (e.g. `SystemVssConnection`).
  #[serde(default)]
  pub endpoints: Vec<JobEndpoint>,
}

/// An endpoint in the job resources.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEndpoint {
  /// The endpoint's name (e.g. `"SystemVssConnection"`).
  pub name: String,
  /// The endpoint's base URL, if reported.
  #[serde(default)]
  pub url: Option<String>,
  /// The endpoint's authorization scheme and parameters, if reported.
  #[serde(default)]
  pub authorization: Option<JobAuthorization>,
  /// Free-form endpoint metadata, keyed by field name (e.g. `FeedStreamUrl`).
  #[serde(default)]
  pub data: HashMap<String, String>,
}

/// Authorization data for an endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobAuthorization {
  /// The authorization scheme (e.g. `"OAuth"`).
  pub scheme: String,
  /// Scheme-specific parameters (e.g. the access token), keyed by name.
  #[serde(default)]
  pub parameters: HashMap<String, String>,
}

/// Workspace options from the job message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOptions {
  /// The workspace clean policy, if specified.
  #[serde(default)]
  pub clean: Option<String>,
}
