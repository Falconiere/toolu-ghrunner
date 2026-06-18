use std::collections::HashMap;

use shared::{Conclusion, RunnerError};

use super::expressions::evaluator::{EvalContext, JobStatus, evaluate};
use super::expressions::template::interpolate;
use super::expressions::types::ExprValue;
use super::secret_masker::SecretMasker;
use super::step_state::{StepState, build_steps_context};

/// Mutable execution state for a job run: context objects, environment,
/// step outputs/conclusions, and job-level status.
pub struct ExecutionContext {
  env: HashMap<String, String>,
  steps: HashMap<String, StepState>,
  github: HashMap<String, ExprValue>,
  runner_context: HashMap<String, ExprValue>,
  job_status: JobStatus,
  secrets: HashMap<String, String>,
  matrix: ExprValue,
  needs: ExprValue,
  inputs: ExprValue,
  masker: SecretMasker,
  path_additions: Vec<String>,
  cgroup_path: Option<std::path::PathBuf>,
}

impl ExecutionContext {
  /// Create a minimal context for unit tests.
  pub fn new_for_test() -> Self {
    let mut runner_ctx = HashMap::new();
    runner_ctx.insert("os".to_owned(), ExprValue::String("Linux".to_owned()));
    runner_ctx.insert("arch".to_owned(), ExprValue::String("X64".to_owned()));
    runner_ctx.insert(
      "name".to_owned(),
      ExprValue::String("test-runner".to_owned()),
    );

    Self {
      env: HashMap::new(),
      steps: HashMap::new(),
      github: HashMap::new(),
      runner_context: runner_ctx,
      job_status: JobStatus::Success,
      secrets: HashMap::new(),
      matrix: ExprValue::Null,
      needs: ExprValue::Null,
      inputs: ExprValue::Null,
      masker: SecretMasker::new(),
      path_additions: Vec::new(),
      cgroup_path: None,
    }
  }

  /// Set the per-job cgroup-v2 directory that spawned steps are moved into.
  pub fn set_cgroup_path(&mut self, path: Option<std::path::PathBuf>) {
    self.cgroup_path = path;
  }

  /// Per-job cgroup directory, if cgroup isolation is active for this run.
  pub fn cgroup_path(&self) -> Option<&std::path::Path> {
    self.cgroup_path.as_deref()
  }

  /// Set a string value in the github context.
  pub fn set_github_context(&mut self, key: &str, value: &str) {
    self
      .github
      .insert(key.to_owned(), ExprValue::String(value.to_owned()));
  }

  /// Set a typed value in the github context (for nested objects like `event`).
  pub fn set_github_context_value(&mut self, key: &str, value: ExprValue) {
    self.github.insert(key.to_owned(), value);
  }

  /// Get a string value from the github context.
  pub fn github_context(&self, key: &str) -> Option<&str> {
    match self.github.get(key) {
      Some(ExprValue::String(s)) => Some(s.as_str()),
      _ => None,
    }
  }

  /// Get a typed value from the github context.
  pub fn github_context_value(&self, key: &str) -> Option<&ExprValue> {
    self.github.get(key)
  }

  /// Build an `EvalContext` snapshot for the expression evaluator.
  pub fn eval_context(&self) -> EvalContext {
    let mut contexts = HashMap::new();

    // github context
    contexts.insert("github".to_owned(), ExprValue::Object(self.github.clone()));

    // env context
    let env_obj: HashMap<String, ExprValue> = self
      .env
      .iter()
      .map(|(k, v)| (k.clone(), ExprValue::String(v.clone())))
      .collect();
    contexts.insert("env".to_owned(), ExprValue::Object(env_obj));

    // steps context
    contexts.insert("steps".to_owned(), build_steps_context(&self.steps));

    // runner context
    contexts.insert(
      "runner".to_owned(),
      ExprValue::Object(self.runner_context.clone()),
    );

    // secrets context
    let secrets_obj: HashMap<String, ExprValue> = self
      .secrets
      .iter()
      .map(|(k, v)| (k.clone(), ExprValue::String(v.clone())))
      .collect();
    contexts.insert("secrets".to_owned(), ExprValue::Object(secrets_obj));

    // matrix, needs, inputs, job, strategy
    contexts.insert("matrix".to_owned(), self.matrix.clone());
    contexts.insert("needs".to_owned(), self.needs.clone());
    contexts.insert("inputs".to_owned(), self.inputs.clone());
    contexts.insert("job".to_owned(), ExprValue::Object(HashMap::new()));
    contexts.insert("strategy".to_owned(), ExprValue::Object(HashMap::new()));

    EvalContext {
      contexts,
      job_status: self.job_status,
    }
  }

  /// # Errors
  /// Returns `RunnerError::Expression` on parse or evaluation failure.
  pub fn evaluate_expression(&self, expr: &str) -> Result<ExprValue, RunnerError> {
    evaluate(expr, &self.eval_context())
  }

  /// # Errors
  /// Returns `RunnerError::Expression` on evaluation failure.
  pub fn interpolate_string(&self, input: &str) -> Result<String, RunnerError> {
    interpolate(input, &self.eval_context())
  }
  pub fn set_env(&mut self, key: &str, value: &str) {
    self.env.insert(key.to_owned(), value.to_owned());
  }

  pub fn prepend_path(&mut self, dir: &str) {
    self.path_additions.push(dir.to_owned());
  }

  pub fn set_step_output(&mut self, step_id: &str, key: &str, value: &str) {
    let state = self
      .steps
      .entry(step_id.to_owned())
      .or_insert_with(|| StepState {
        outputs: HashMap::new(),
        outcome: None,
      });
    state.outputs.insert(key.to_owned(), value.to_owned());
  }

  pub fn set_step_conclusion(&mut self, step_id: &str, conclusion: Conclusion) {
    let state = self
      .steps
      .entry(step_id.to_owned())
      .or_insert_with(|| StepState {
        outputs: HashMap::new(),
        outcome: None,
      });
    state.outcome = Some(conclusion);
  }

  pub fn record_step_failure(&mut self) {
    self.job_status = JobStatus::Failure;
  }

  pub fn job_status(&self) -> JobStatus {
    self.job_status
  }

  pub fn masker(&self) -> &SecretMasker {
    &self.masker
  }

  /// Register a secret variable and add to masker.
  pub fn register_secret(&mut self, key: &str, value: &str) {
    self.secrets.insert(key.to_owned(), value.to_owned());
    self.masker.add_secret(value);
  }

  /// Add a mask hint value to the masker.
  pub fn add_mask(&mut self, value: &str) {
    self.masker.add_secret(value);
  }

  /// Merge global env + step env + PATH additions into a full env map.
  pub fn build_step_env(&self, step_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut result = self.env.clone();
    result.extend(step_env.clone());

    // Prepend path additions (reverse order) to existing PATH
    if !self.path_additions.is_empty() {
      let existing = result.get("PATH").cloned().unwrap_or_default();
      let mut new_path: Vec<&str> = self
        .path_additions
        .iter()
        .rev()
        .map(String::as_str)
        .collect();
      if !existing.is_empty() {
        new_path.push(&existing);
      }
      result.insert("PATH".to_owned(), new_path.join(":"));
    }

    result
  }
}
