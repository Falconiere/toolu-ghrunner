//! Raw serde deserialization types for workflow YAML.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct RawWorkflow {
  pub(super) name: Option<String>,
  #[serde(rename = "on")]
  pub(super) on: Option<serde_yaml::Value>,
  pub(super) env: Option<HashMap<String, String>>,
  pub(super) permissions: Option<serde_yaml::Value>,
  pub(super) jobs: Option<HashMap<String, RawJob>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct RawJob {
  pub(super) runs_on: Option<serde_yaml::Value>,
  pub(super) needs: Option<serde_yaml::Value>,
  #[serde(rename = "if")]
  pub(super) if_condition: Option<String>,
  pub(super) env: Option<HashMap<String, String>>,
  pub(super) permissions: Option<serde_yaml::Value>,
  pub(super) strategy: Option<RawStrategy>,
  pub(super) steps: Option<Vec<RawStep>>,
  pub(super) outputs: Option<HashMap<String, String>>,
  pub(super) container: Option<serde_yaml::Value>,
  pub(super) services: Option<serde_yaml::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct RawStrategy {
  pub(super) matrix: Option<serde_yaml::Value>,
  pub(super) fail_fast: Option<bool>,
  pub(super) max_parallel: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct RawStep {
  pub(super) id: Option<String>,
  pub(super) name: Option<String>,
  pub(super) uses: Option<String>,
  pub(super) run: Option<String>,
  pub(super) shell: Option<String>,
  pub(super) with: Option<HashMap<String, String>>,
  pub(super) env: Option<HashMap<String, String>>,
  #[serde(rename = "if")]
  pub(super) if_condition: Option<String>,
  pub(super) continue_on_error: Option<bool>,
  pub(super) timeout_minutes: Option<u32>,
  pub(super) working_directory: Option<String>,
}
