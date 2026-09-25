//! Job-level execution inputs that live above the per-step loop: the job's
//! `outputs:` expression map and the merged `defaults.run` (shell +
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
/// Holds the final mapping from ordered workflow/job `defaults.run`. Steps that set
/// their own `shell:`/`working-directory:` still win over both (applied at the
/// step site).
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
