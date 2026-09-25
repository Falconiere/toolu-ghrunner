//! Job-level execution inputs that live above the per-step loop: the job's
//! `outputs:` expression map and the resolved `defaults.run` (shell +
//! working-directory) fallback.
//!
//! Ordered `defaults` mappings from the acquired message supply the live
//! run-step fallback. Job outputs are wired separately.

use std::collections::HashMap;

use shared::{RunnerError, TemplateToken};

use super::context::ExecutionContext;
use super::step_env::env_token_to_string;
use super::workflow::types::WorkflowDefaults;

/// Job-level fallback shell + working-directory for `run:` steps.
///
/// Holds the final mapping from ordered workflow/job `defaults.run`. Each later
/// run mapping replaces the preceding mapping, even when it omits a key. Steps
/// that set their own `shell:`/`working-directory:` still win at the step site.
#[derive(Debug, Clone, Default)]
pub struct RunDefaultsResolved {
  /// Fallback shell name (e.g. `bash`) when a run-step omits `shell:`.
  pub shell: Option<String>,
  /// Fallback working directory when a run-step omits `working-directory:`.
  pub working_directory: Option<String>,
}

/// Constant-per-job inputs threaded into the step loop and consumed at job end.
#[derive(Debug, Clone, Default)]
pub struct JobSpec {
  /// Job-level `outputs:` map: name → `${{ }}` expression string. Evaluated
  /// against the final context after all steps + post-steps complete.
  pub outputs: HashMap<String, String>,
  /// Ordered, unevaluated `jobOutputs` token from the acquired job message.
  /// This takes precedence over locally parsed workflow outputs when present.
  pub acquired_outputs: Option<TemplateToken>,
  /// Final `defaults.run` mapping applied to run-steps that omit their own.
  pub defaults: RunDefaultsResolved,
}

impl JobSpec {
  /// Build a `JobSpec` from parsed workflow data for local workflow callers.
  /// A present job `run` mapping replaces the whole workflow `run` mapping.
  pub fn from_workflow(
    outputs: HashMap<String, String>,
    workflow_defaults: Option<&WorkflowDefaults>,
    job_defaults: Option<&WorkflowDefaults>,
  ) -> Self {
    Self {
      outputs,
      acquired_outputs: None,
      defaults: merge_run_defaults(workflow_defaults, job_defaults),
    }
  }

  /// Evaluate ordered run-default mappings from a captured job message.
  ///
  /// # Errors
  ///
  /// Returns a protocol or expression error for malformed mapping tokens or
  /// a default whose expression cannot be evaluated.
  pub fn from_message_defaults(
    defaults: &[TemplateToken],
    ctx: &ExecutionContext,
  ) -> Result<RunDefaultsResolved, RunnerError> {
    let eval_ctx = ctx.eval_context();
    let mut resolved = RunDefaultsResolved::default();
    for layer in defaults {
      for (name, token) in mapping_entries(layer, "defaults")? {
        if name.eq_ignore_ascii_case("run") {
          resolved = parse_run_mapping(token, ctx, &eval_ctx)?;
        }
      }
    }
    Ok(resolved)
  }
}

/// Evaluated acquired outputs, including suppression and evaluation failures.
pub(super) struct AcquiredOutputResult {
  /// Safe values to send to the Run Service, including values before an error.
  pub(super) outputs: HashMap<String, String>,
  /// Names of secret-bearing output values omitted from the result.
  pub(super) skipped_secret_names: Vec<String>,
  /// The first malformed token or expression error, if any.
  pub(super) error: Option<RunnerError>,
}

enum AcquiredValue {
  Empty,
  Secret,
  Value(String),
}

/// Evaluate acquired job outputs in their wire order against the final context.
///
/// The upstream runner skips empty and secret-bearing values. It retains values
/// accepted before an evaluation error, which then fails the job. Names are
/// compared without ASCII case so duplicate names follow the upstream map.
pub(super) fn evaluate_acquired_outputs(
  token: &TemplateToken,
  ctx: &ExecutionContext,
) -> AcquiredOutputResult {
  let mut result = AcquiredOutputResult {
    outputs: HashMap::new(),
    skipped_secret_names: Vec::new(),
    error: None,
  };
  let entries = match mapping_entries(token, "jobOutputs") {
    Ok(entries) => entries,
    Err(error) => {
      result.error = Some(error);
      return result;
    },
  };
  let eval_ctx = ctx.eval_context();
  for (name, value_token) in entries {
    if name.is_empty() {
      continue;
    }
    match evaluate_acquired_value(name, value_token, ctx, &eval_ctx) {
      Ok(AcquiredValue::Empty) => {},
      Ok(AcquiredValue::Secret) => result.skipped_secret_names.push(name.to_owned()),
      Ok(AcquiredValue::Value(value)) => insert_output(&mut result.outputs, name, value),
      Err(error) => {
        result.error = Some(error);
        break;
      },
    }
  }
  result
}

fn evaluate_acquired_value(
  name: &str,
  token: &TemplateToken,
  ctx: &ExecutionContext,
  eval_ctx: &expressions::evaluator::EvalContext,
) -> Result<AcquiredValue, RunnerError> {
  if !matches!(token.token_type, 0 | 3 | 5 | 6 | 7) {
    return Err(RunnerError::Protocol(format!(
      "jobOutputs.{name} must be a scalar token"
    )));
  }
  let value = env_token_to_string(token, ctx, eval_ctx)?;
  if value.is_empty() {
    return Ok(AcquiredValue::Empty);
  }
  let guard = match ctx.masker().lock() {
    Ok(guard) => guard,
    Err(poisoned) => poisoned.into_inner(),
  };
  if guard.mask(&value).as_ref() != value {
    Ok(AcquiredValue::Secret)
  } else {
    Ok(AcquiredValue::Value(value))
  }
}

fn insert_output(outputs: &mut HashMap<String, String>, name: &str, value: String) {
  if let Some(existing) = outputs
    .keys()
    .find(|key| key.eq_ignore_ascii_case(name))
    .cloned()
  {
    outputs.insert(existing, value);
  } else {
    outputs.insert(name.to_owned(), value);
  }
}

fn mapping_entries<'a>(
  token: &'a TemplateToken,
  path: &str,
) -> Result<Vec<(&'a str, &'a TemplateToken)>, RunnerError> {
  if token.token_type != 2 {
    return Err(RunnerError::Protocol(format!(
      "{path} must be a mapping token"
    )));
  }
  let entries = token
    .d
    .as_deref()
    .ok_or_else(|| RunnerError::Protocol(format!("{path} mapping has no entries")))?;
  entries
    .iter()
    .map(|entry| {
      entry
        .key
        .to_string_value()
        .map(|name| (name, &entry.value))
        .ok_or_else(|| RunnerError::Protocol(format!("{path} key must be literal")))
    })
    .collect()
}

fn parse_run_mapping(
  token: &TemplateToken,
  ctx: &ExecutionContext,
  eval_ctx: &expressions::evaluator::EvalContext,
) -> Result<RunDefaultsResolved, RunnerError> {
  let mut resolved = RunDefaultsResolved::default();
  for (name, value) in mapping_entries(token, "defaults.run")? {
    if name.eq_ignore_ascii_case("shell") {
      resolved.shell = parse_value(value, "shell", ctx, eval_ctx)?;
    } else if name.eq_ignore_ascii_case("working-directory") {
      resolved.working_directory = parse_value(value, "working-directory", ctx, eval_ctx)?;
    }
  }
  Ok(resolved)
}

fn parse_value(
  token: &TemplateToken,
  name: &str,
  ctx: &ExecutionContext,
  eval_ctx: &expressions::evaluator::EvalContext,
) -> Result<Option<String>, RunnerError> {
  // The generic step-env converter warns and returns an empty string for
  // non-scalars; acquired defaults must reject that malformed wire value.
  if !matches!(token.token_type, 0 | 3 | 5 | 6 | 7) {
    return Err(RunnerError::Protocol(format!(
      "defaults.run.{name} must be a scalar token"
    )));
  }
  let value = env_token_to_string(token, ctx, eval_ctx)?;
  Ok((!value.is_empty()).then_some(value))
}

/// Merge workflow + job `defaults.run` with whole-mapping job precedence.
fn merge_run_defaults(
  workflow: Option<&WorkflowDefaults>,
  job: Option<&WorkflowDefaults>,
) -> RunDefaultsResolved {
  let run = job
    .and_then(|defaults| defaults.run.as_ref())
    .or_else(|| workflow.and_then(|defaults| defaults.run.as_ref()));
  RunDefaultsResolved {
    shell: run
      .and_then(|r| r.shell.clone())
      .filter(|value| !value.is_empty()),
    working_directory: run
      .and_then(|r| r.working_directory.clone())
      .filter(|value| !value.is_empty()),
  }
}

/// Evaluate the job's `outputs:` map against the final execution context.
///
/// Each value is a `${{ }}` expression (typically `steps.<id>.outputs.<k>`).
/// Interpolation runs after all main + post steps complete, so step outputs are
/// fully recorded. The resolved map is placed into `JobCompleted.outputs`.
///
/// # Errors
///
/// Returns `RunnerError::Expression` if an output expression fails to evaluate.
pub fn evaluate_job_outputs(
  spec: &JobSpec,
  ctx: &ExecutionContext,
) -> Result<HashMap<String, String>, RunnerError> {
  let mut resolved = HashMap::with_capacity(spec.outputs.len());
  for (name, expr) in &spec.outputs {
    let value = ctx.interpolate_string(expr)?;
    resolved.insert(name.clone(), value);
  }
  Ok(resolved)
}
