use std::collections::HashMap;

use serde::Deserialize;

/// A parsed workflow definition.
#[derive(Debug, Clone)]
pub struct WorkflowDefinition {
  pub name: Option<String>,
  pub on: TriggerConfig,
  pub env: HashMap<String, String>,
  pub defaults: Option<WorkflowDefaults>,
  pub permissions: Option<serde_yaml::Value>,
  pub jobs: HashMap<String, JobDefinition>,
}

/// Trigger configuration from the `on:` section.
#[derive(Debug, Clone, Default)]
pub struct TriggerConfig {
  pub push: Option<BranchFilter>,
  pub pull_request: Option<BranchFilter>,
  pub workflow_dispatch: Option<serde_yaml::Value>,
  pub schedule: Option<Vec<serde_yaml::Value>>,
  pub event_names: Vec<String>,
}

/// Branch/path/tag filter for push/pull_request triggers.
#[derive(Debug, Clone, Default)]
pub struct BranchFilter {
  pub branches: Vec<String>,
  pub tags: Vec<String>,
  pub paths: Vec<String>,
}

/// Default settings for run steps.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowDefaults {
  pub run: Option<RunDefaults>,
}

/// Default shell and working directory for run steps.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunDefaults {
  pub shell: Option<String>,
  pub working_directory: Option<String>,
}

/// A job definition within a workflow.
#[derive(Debug, Clone)]
pub struct JobDefinition {
  pub runs_on: Vec<String>,
  pub needs: Vec<String>,
  pub if_condition: Option<String>,
  pub env: HashMap<String, String>,
  pub defaults: Option<WorkflowDefaults>,
  pub permissions: Option<serde_yaml::Value>,
  pub strategy: Option<StrategyConfig>,
  pub steps: Vec<StepDefinition>,
  pub outputs: HashMap<String, String>,
  pub container: Option<serde_yaml::Value>,
  pub services: Option<serde_yaml::Value>,
}

/// Matrix strategy configuration.
#[derive(Debug, Clone)]
pub struct StrategyConfig {
  pub matrix: MatrixConfig,
  pub fail_fast: bool,
  pub max_parallel: Option<u32>,
}

/// Matrix configuration with base keys, include, and exclude.
#[derive(Debug, Clone, Default)]
pub struct MatrixConfig {
  pub base: HashMap<String, Vec<serde_yaml::Value>>,
  pub include: Vec<HashMap<String, serde_yaml::Value>>,
  pub exclude: Vec<HashMap<String, serde_yaml::Value>>,
}

/// A step definition within a job.
#[derive(Debug, Clone)]
pub struct StepDefinition {
  pub id: Option<String>,
  pub name: Option<String>,
  pub uses: Option<String>,
  pub run: Option<String>,
  pub shell: Option<String>,
  pub with: HashMap<String, String>,
  pub env: HashMap<String, String>,
  pub if_condition: Option<String>,
  pub continue_on_error: bool,
  pub timeout_minutes: Option<u32>,
  pub working_directory: Option<String>,
}
