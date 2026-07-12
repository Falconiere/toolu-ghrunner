use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use shared::platform::{runner_arch, runner_os};
use shared::{Conclusion, RunnerError, SecretMasker};

use super::context_build::{build_strategy, default_strategy, runner_debug_on};
use expressions::evaluator::{EvalContext, JobStatus, evaluate};
use expressions::template::interpolate;
use expressions::types::ExprValue;
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
  /// Repo/org/env configuration variables — the `vars.*` context.
  vars: HashMap<String, String>,
  matrix: ExprValue,
  needs: ExprValue,
  inputs: ExprValue,
  /// `strategy.*` context (job-index/total, fail-fast, max-parallel).
  strategy: HashMap<String, ExprValue>,
  /// Shared with the listener and the tracing file sink's redactor.
  /// Wrapped in a `Mutex` so `register_secret` and `add_mask` can
  /// mutate the pattern set from any Arc clone — the file sink's
  /// `MaskerRedactor` reads the same Mutex on every line, so
  /// registrations are visible to the file sink on the very next
  /// `redact` call.
  masker: Arc<Mutex<SecretMasker>>,
  path_additions: Vec<String>,
  cgroup_path: Option<std::path::PathBuf>,
  /// Per-job workspace root; `hashFiles()` resolves its patterns against it.
  workspace: Option<std::path::PathBuf>,
}

impl ExecutionContext {
  /// Create a minimal context for unit tests with its own private masker.
  pub fn new_for_test() -> Self {
    Self::with_masker(Arc::new(Mutex::new(SecretMasker::new())))
  }

  /// Create a context that shares `masker` with the caller (typically
  /// the listener, which also shares the same Arc with the tracing
  /// file sink's redactor).
  pub fn with_masker(masker: Arc<Mutex<SecretMasker>>) -> Self {
    let mut runner_ctx = HashMap::new();
    runner_ctx.insert("os".to_owned(), ExprValue::String(runner_os().to_owned()));
    runner_ctx.insert(
      "arch".to_owned(),
      ExprValue::String(runner_arch().to_owned()),
    );

    Self {
      env: HashMap::new(),
      steps: HashMap::new(),
      github: HashMap::new(),
      runner_context: runner_ctx,
      job_status: JobStatus::Success,
      secrets: HashMap::new(),
      vars: HashMap::new(),
      matrix: ExprValue::Null,
      needs: ExprValue::Null,
      inputs: ExprValue::Null,
      strategy: default_strategy(),
      masker,
      path_additions: Vec::new(),
      cgroup_path: None,
      workspace: None,
    }
  }

  /// Set the per-job workspace root that `hashFiles()` resolves against.
  pub fn set_workspace(&mut self, path: Option<std::path::PathBuf>) {
    self.workspace = path;
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

  /// Populate the host/config-derived `runner.*` context and mirror it to env.
  ///
  /// `os`/`arch` come from the host (Linux target only — see non-goals);
  /// `name` is the registered runner name (falling back to the message's
  /// runner dict, then hostname); `temp`/`tool_cache` are pinned to
  /// `data_dir/_temp` and `data_dir/_tool` (Open Q6) and created if absent;
  /// `debug` is `"1"` when step-debug is on, else unset. Mirrors
  /// `RUNNER_OS`/`RUNNER_ARCH`/`RUNNER_NAME`/`RUNNER_TEMP`/`RUNNER_TOOL_CACHE`.
  ///
  /// # Errors
  /// Returns `RunnerError::Io` if `temp`/`tool_cache` cannot be created.
  pub fn set_runner_context(
    &mut self,
    name: &str,
    data_dir: &std::path::Path,
  ) -> Result<(), RunnerError> {
    let temp = data_dir.join("_temp");
    let tool_cache = data_dir.join("_tool");
    std::fs::create_dir_all(&temp)?;
    std::fs::create_dir_all(&tool_cache)?;
    restrict_dir_permissions(&temp)?;
    restrict_dir_permissions(&tool_cache)?;
    let temp = temp.to_string_lossy().into_owned();
    let tool_cache = tool_cache.to_string_lossy().into_owned();

    let os = runner_os().to_owned();
    let arch = runner_arch().to_owned();
    self.set_runner_value("os", &os);
    self.set_runner_value("arch", &arch);
    self.set_runner_value("name", name);
    self.set_runner_value("temp", &temp);
    self.set_runner_value("tool_cache", &tool_cache);

    self.set_env("RUNNER_OS", &os);
    self.set_env("RUNNER_ARCH", &arch);
    self.set_env("RUNNER_NAME", name);
    self.set_env("RUNNER_TEMP", &temp);
    self.set_env("RUNNER_TOOL_CACHE", &tool_cache);

    if runner_debug_on() {
      self.set_runner_value("debug", "1");
      self.set_env("RUNNER_DEBUG", "1");
    }
    Ok(())
  }

  fn set_runner_value(&mut self, key: &str, value: &str) {
    self
      .runner_context
      .insert(key.to_owned(), ExprValue::String(value.to_owned()));
  }

  /// Set a repo/org/env configuration variable (`vars.*`).
  pub fn set_var(&mut self, key: &str, value: &str) {
    self.vars.insert(key.to_owned(), value.to_owned());
  }

  /// Populate the `strategy.*` context (job-index/total, fail-fast,
  /// max-parallel). Absent matrix/strategy keeps the single-job defaults.
  pub fn set_strategy(
    &mut self,
    job_index: u64,
    job_total: u64,
    fail_fast: bool,
    max_parallel: Option<u64>,
  ) {
    self.strategy = build_strategy(job_index, job_total, fail_fast, max_parallel);
  }
}

/// Restrict a runner-owned directory to the runner user (`0o700`): `_temp`
/// holds step scripts and event payloads that can embed secrets, so it must
/// not be world-readable under a permissive umask. Also applied to the
/// enclosing `data_dir` by `job_runner` so the leaf tightening is not undone
/// by a world-readable parent. No-op on non-Unix targets.
pub(crate) fn restrict_dir_permissions(dir: &std::path::Path) -> std::io::Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
  }
  #[cfg(not(unix))]
  {
    let _ = dir;
  }
  Ok(())
}

/// Context snapshot + expression evaluation + env accessors.
impl ExecutionContext {
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

    // secrets context (from job Variables where is_secret == true)
    contexts.insert(
      "secrets".to_owned(),
      ExprValue::Object(string_map_to_obj(&self.secrets)),
    );

    // vars context (repo/org/env configuration variables)
    contexts.insert(
      "vars".to_owned(),
      ExprValue::Object(string_map_to_obj(&self.vars)),
    );

    // matrix, needs, inputs, job, strategy
    contexts.insert("matrix".to_owned(), self.matrix.clone());
    contexts.insert("needs".to_owned(), self.needs.clone());
    contexts.insert("inputs".to_owned(), self.inputs.clone());
    contexts.insert("job".to_owned(), ExprValue::Object(self.job_context()));
    contexts.insert(
      "strategy".to_owned(),
      ExprValue::Object(self.strategy.clone()),
    );

    EvalContext {
      contexts,
      job_status: self.job_status,
      workspace: self.workspace.clone(),
    }
  }

  /// Build the `job.*` context: a real `status` plus empty container/services
  /// (no-container jobs are in scope; container objects stay null).
  fn job_context(&self) -> HashMap<String, ExprValue> {
    let mut job = HashMap::new();
    job.insert(
      "status".to_owned(),
      ExprValue::String(job_status_str(self.job_status).to_owned()),
    );
    job.insert("container".to_owned(), ExprValue::Null);
    job.insert("services".to_owned(), ExprValue::Object(HashMap::new()));
    job
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
  /// Set (or overwrite) a global environment variable for later steps.
  pub fn set_env(&mut self, key: &str, value: &str) {
    self.env.insert(key.to_owned(), value.to_owned());
  }

  /// Read a job-level environment variable (e.g. a job-hook script path).
  pub fn env_var(&self, key: &str) -> Option<String> {
    self.env.get(key).cloned()
  }

  /// Prepend a directory to `PATH` for subsequent steps.
  pub fn prepend_path(&mut self, dir: &str) {
    self.path_additions.push(dir.to_owned());
  }
}

/// Per-step output / state / conclusion recording.
impl ExecutionContext {
  /// Record an output value for a step (`set-output` / `$GITHUB_OUTPUT`).
  pub fn set_step_output(&mut self, step_id: &str, key: &str, value: &str) {
    let state = self.steps.entry(step_id.to_owned()).or_default();
    state.outputs.insert(key.to_owned(), value.to_owned());
  }

  /// Record a `save-state` value for a step, surfaced to its post step.
  pub fn set_step_state(&mut self, step_id: &str, key: &str, value: &str) {
    let state = self.steps.entry(step_id.to_owned()).or_default();
    state.state.insert(key.to_owned(), value.to_owned());
  }

  /// Read the recorded outputs map for a step (empty if none).
  pub fn step_outputs(&self, step_id: &str) -> HashMap<String, String> {
    self
      .steps
      .get(step_id)
      .map(|s| s.outputs.clone())
      .unwrap_or_default()
  }

  /// Read the recorded `save-state` map for a step, surfaced as `STATE_*`
  /// to that same step's pre/main/post stages (empty if none).
  pub fn step_state(&self, step_id: &str) -> HashMap<String, String> {
    self
      .steps
      .get(step_id)
      .map(|s| s.state.clone())
      .unwrap_or_default()
  }

  /// Record a step's REAL result (`steps.<id>.outcome`), before any
  /// `continue-on-error` adjustment.
  pub fn set_step_outcome(&mut self, step_id: &str, outcome: Conclusion) {
    let state = self.steps.entry(step_id.to_owned()).or_default();
    state.outcome = Some(outcome);
  }

  /// Record a step's effective result (`steps.<id>.conclusion`), after
  /// `continue-on-error` (equals the outcome unless the step failed with
  /// `continue-on-error: true`).
  pub fn set_step_conclusion(&mut self, step_id: &str, conclusion: Conclusion) {
    let state = self.steps.entry(step_id.to_owned()).or_default();
    state.conclusion = Some(conclusion);
  }

  /// Mark the overall job status as failed (for `failure()` conditions).
  pub fn record_step_failure(&mut self) {
    self.job_status = JobStatus::Failure;
  }

  /// Current aggregate job status used by step-condition functions.
  pub fn job_status(&self) -> JobStatus {
    self.job_status
  }

  /// Borrow the shared secret masker (same `Arc` as the tracing redactor).
  pub fn masker(&self) -> &Arc<Mutex<SecretMasker>> {
    &self.masker
  }

  /// Register a secret variable and add to the shared masker.
  ///
  /// The shared masker is wrapped in a `Mutex` so a single
  /// `&mut ExecutionContext` is enough to register a new secret —
  /// the file sink's redactor reads the same Mutex on every log
  /// line, so the new secret is visible to the file sink on the
  /// very next `redact` call.
  pub fn register_secret(&mut self, key: &str, value: &str) {
    self.secrets.insert(key.to_owned(), value.to_owned());
    // Recover from a poisoned Mutex without the panic-on-poison
    // convenience, since this codebase's `no-unwrap` gate forbids
    // the `Result::expect` method. If a prior holder panicked, the
    // inner SecretMasker is still valid.
    let mut guard = match self.masker.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    guard.add_secret(value);
  }

  /// Register a value with the shared masker only, without exposing it in the
  /// `secrets.*` context. Used for the auto github token, which is masked but
  /// surfaced as `github.token` (matches actions/runner's exclusion).
  pub fn register_secret_masked(&mut self, value: &str) {
    self.add_mask(value);
  }

  /// Add a mask hint value to the shared masker.
  pub fn add_mask(&mut self, value: &str) {
    let mut guard = match self.masker.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    guard.add_secret(value);
  }

  /// Merge global env + step env + PATH additions into a full env map.
  pub fn build_step_env(&self, step_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut result = self.env.clone();
    result.extend(step_env.clone());

    // Prepend path additions (reverse order) to existing PATH. The job env
    // rarely carries PATH itself, so fall back to the process PATH — without
    // it the step env's PATH would be ONLY the additions, the spawn-time env
    // override would clobber the inherited PATH, and the step shell (`bash`)
    // becomes unresolvable (live bug: every run-step after setup-node
    // failed with ENOENT). The fallback is read lazily per step; steps
    // run as child processes and cannot mutate this process's PATH, so
    // it is stable for the life of the (single-job) run.
    if !self.path_additions.is_empty() {
      let existing = result
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
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

/// Map a `String → String` table to an `ExprValue::Object` of strings.
fn string_map_to_obj(map: &HashMap<String, String>) -> HashMap<String, ExprValue> {
  map
    .iter()
    .map(|(k, v)| (k.clone(), ExprValue::String(v.clone())))
    .collect()
}

/// `job.status` string form of the current aggregate job status.
fn job_status_str(status: JobStatus) -> &'static str {
  match status {
    JobStatus::Success => "success",
    JobStatus::Failure => "failure",
    JobStatus::Cancelled => "cancelled",
  }
}
