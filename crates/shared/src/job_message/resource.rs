//! Job resource types: variables, masking, endpoints, authorization, workspace.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A variable in the job message (may be secret).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableValue {
  pub value: String,
  #[serde(default)]
  pub is_secret: bool,
}

/// Hint for values that should be masked in logs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskHint {
  pub value: String,
  #[serde(default, rename = "type")]
  pub mask_type: Option<String>,
}

/// Resources available to the job (endpoints, authorizations).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResources {
  #[serde(default)]
  pub endpoints: Vec<JobEndpoint>,
}

/// An endpoint in the job resources.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEndpoint {
  pub name: String,
  #[serde(default)]
  pub url: Option<String>,
  #[serde(default)]
  pub authorization: Option<JobAuthorization>,
  #[serde(default)]
  pub data: HashMap<String, String>,
}

/// Authorization data for an endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobAuthorization {
  pub scheme: String,
  #[serde(default)]
  pub parameters: HashMap<String, String>,
}

/// Workspace options from the job message.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOptions {
  #[serde(default)]
  pub clean: Option<String>,
}
