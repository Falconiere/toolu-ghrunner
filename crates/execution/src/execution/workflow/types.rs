use std::collections::HashMap;

use serde::Deserialize;

/// A parsed workflow definition.
#[derive(Debug, Clone)]
pub struct WorkflowDefinition {
  /// Workflow display name.
  pub name: Option<String>,
  /// The `on:` trigger configuration.
  pub on: TriggerConfig,
  /// Workflow-level `env:` entries.
  pub env: HashMap<String, String>,
  /// Workflow-level `defaults:`.
  pub defaults: Option<WorkflowDefaults>,
  /// Workflow-level `permissions:` (opaque; not yet enforced).
  pub permissions: Option<serde_yaml::Value>,
  /// Jobs keyed by job id.
  pub jobs: HashMap<String, JobDefinition>,
}

/// Trigger configuration from the `on:` section.
#[derive(Debug, Clone, Default)]
pub struct TriggerConfig {
  /// `push:` filter, if the workflow triggers on push.
  pub push: Option<BranchFilter>,
  /// `pull_request:` filter, if the workflow triggers on pull request.
  pub pull_request: Option<BranchFilter>,
  /// `workflow_dispatch:` config (opaque; inputs read separately).
  pub workflow_dispatch: Option<serde_yaml::Value>,
  /// `schedule:` cron entries.
  pub schedule: Option<Vec<serde_yaml::Value>>,
  /// All event names this workflow listens for.
  pub event_names: Vec<String>,
}

/// Branch/path/tag filter for `push`/`pull_request` triggers.
#[derive(Debug, Clone, Default)]
pub struct BranchFilter {
  /// Branch patterns to match.
  pub branches: Vec<String>,
  /// Tag patterns to match.
  pub tags: Vec<String>,
  /// Path patterns to match against changed files.
  pub paths: Vec<String>,
}

/// Default settings for run steps.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowDefaults {
  /// The `run:` defaults (shell + working directory).
  pub run: Option<RunDefaults>,
}

/// Default shell and working directory for run steps.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunDefaults {
  /// Default shell for `run:` steps.
  pub shell: Option<String>,
  /// Default working directory for `run:` steps.
  pub working_directory: Option<String>,
}

/// A job definition within a workflow.
#[derive(Debug, Clone)]
pub struct JobDefinition {
  /// `runs-on:` labels selecting the runner.
  pub runs_on: Vec<String>,
  /// Job ids this job depends on (`needs:`).
  pub needs: Vec<String>,
  /// `if:` expression gating whether the job runs.
  pub if_condition: Option<String>,
  /// Job-level `env:` entries.
  pub env: HashMap<String, String>,
  /// Job-level `defaults:`.
  pub defaults: Option<WorkflowDefaults>,
  /// Job-level `permissions:` (opaque; not yet enforced).
  pub permissions: Option<serde_yaml::Value>,
  /// `strategy:` (matrix build) configuration, if any.
  pub strategy: Option<StrategyConfig>,
  /// The job's `steps:` array.
  pub steps: Vec<StepDefinition>,
  /// The job's declared `outputs:` expressions.
  pub outputs: HashMap<String, String>,
  /// `container:` spec (opaque; not yet enforced).
  pub container: Option<serde_yaml::Value>,
  /// `services:` spec (opaque; not yet enforced).
  pub services: Option<serde_yaml::Value>,
}

/// Matrix strategy configuration.
#[derive(Debug, Clone)]
pub struct StrategyConfig {
  /// The build matrix to expand into job instances.
  pub matrix: MatrixConfig,
  /// Whether one failing matrix job cancels the others.
  pub fail_fast: bool,
  /// Maximum number of matrix jobs to run concurrently.
  pub max_parallel: Option<u32>,
}

/// Matrix configuration with base keys, include, and exclude.
#[derive(Debug, Clone, Default)]
pub struct MatrixConfig {
  /// Base matrix dimensions keyed by variable name.
  pub base: HashMap<String, Vec<serde_yaml::Value>>,
  /// Additional combinations to include.
  pub include: Vec<HashMap<String, serde_yaml::Value>>,
  /// Combinations to exclude from the expanded matrix.
  pub exclude: Vec<HashMap<String, serde_yaml::Value>>,
}

/// A step definition within a job.
#[derive(Debug, Clone)]
pub struct StepDefinition {
  /// Step id, used to reference its outputs from other steps.
  pub id: Option<String>,
  /// Display name for the step.
  pub name: Option<String>,
  /// Action reference, for a `uses:` step.
  pub uses: Option<String>,
  /// Shell script body, for a `run:` step.
  pub run: Option<String>,
  /// Shell to run the script under.
  pub shell: Option<String>,
  /// `with:` inputs passed to a `uses:` step.
  pub with: HashMap<String, String>,
  /// `env:` entries for the step.
  pub env: HashMap<String, String>,
  /// `if:` expression gating whether the step runs.
  pub if_condition: Option<String>,
  /// Whether a failure in this step should not fail the job.
  pub continue_on_error: bool,
  /// `timeout-minutes` bound for the step.
  pub timeout_minutes: Option<u32>,
  /// `working-directory` override for the step.
  pub working_directory: Option<String>,
}
