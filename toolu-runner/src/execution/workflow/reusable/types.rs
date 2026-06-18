//! Type definitions and validation/resolution logic for reusable workflows.

use std::collections::HashMap;

use shared::RunnerError;

/// Parsed reference to a reusable workflow.
///
/// Format: `{owner}/{repo}/{path}@{ref}`
#[derive(Debug, Clone)]
pub struct ReusableWorkflowRef {
  pub owner: String,
  pub repo: String,
  pub path: String,
  pub git_ref: String,
}

/// Definition of a reusable workflow input from `on.workflow_call.inputs`.
#[derive(Debug, Clone)]
pub struct InputDef {
  pub required: bool,
  pub default: Option<String>,
}

/// Definition of a reusable workflow secret from `on.workflow_call.secrets`.
#[derive(Debug, Clone)]
pub struct SecretDef {
  pub required: bool,
}

/// How the caller passes secrets to the reusable workflow.
#[derive(Debug, Clone)]
pub enum SecretMode {
  /// `secrets: inherit` -- pass all caller secrets through.
  Inherit,
  /// Explicit mapping: `secrets: { KEY: value }`.
  Explicit(HashMap<String, String>),
}

/// Definition of a reusable workflow output.
#[derive(Debug, Clone)]
pub struct OutputDef {
  pub description: Option<String>,
  pub value: String,
}

/// Parsed definition of a reusable workflow from `on: workflow_call:`.
#[derive(Debug, Clone)]
pub struct ReusableWorkflowDef {
  pub inputs: HashMap<String, InputDef>,
  pub outputs: HashMap<String, OutputDef>,
  pub secrets: HashMap<String, SecretDef>,
}

/// Context passed from a caller workflow to a reusable workflow.
#[derive(Debug, Clone)]
pub struct CallerContext {
  pub inputs: HashMap<String, String>,
  pub secrets: HashMap<String, String>,
}

/// Validate that all required inputs are provided by the caller.
///
/// # Errors
///
/// Returns `RunnerError::ReusableWorkflow` if a required input is missing.
pub fn validate_inputs(
  defined: &HashMap<String, InputDef>,
  caller_inputs: &HashMap<String, String>,
) -> Result<(), RunnerError> {
  for (name, def) in defined {
    if def.required && def.default.is_none() && !caller_inputs.contains_key(name) {
      return Err(RunnerError::ReusableWorkflow(format!(
        "required input '{name}' not provided by caller"
      )));
    }
  }
  Ok(())
}

/// Build the resolved inputs map: caller values override defaults.
pub fn resolve_inputs(
  defined: &HashMap<String, InputDef>,
  caller_inputs: &HashMap<String, String>,
) -> HashMap<String, String> {
  let mut resolved = HashMap::new();
  for (name, def) in defined {
    if let Some(caller_value) = caller_inputs.get(name) {
      resolved.insert(name.clone(), caller_value.clone());
    } else if let Some(default) = &def.default {
      resolved.insert(name.clone(), default.clone());
    }
  }
  resolved
}

/// Validate and resolve secrets according to the secret mode.
///
/// # Errors
///
/// Returns `RunnerError::ReusableWorkflow` if a required secret is missing.
pub fn validate_secrets(
  mode: &SecretMode,
  defined: &HashMap<String, SecretDef>,
  caller_secrets: &HashMap<String, String>,
) -> Result<HashMap<String, String>, RunnerError> {
  match mode {
    SecretMode::Inherit => Ok(caller_secrets.clone()),
    SecretMode::Explicit(mapping) => {
      for (name, def) in defined {
        if def.required && !mapping.contains_key(name) {
          return Err(RunnerError::ReusableWorkflow(format!(
            "required secret '{name}' not provided by caller"
          )));
        }
      }
      Ok(mapping.clone())
    },
  }
}

/// Resolve reusable workflow outputs from job execution results.
///
/// Output `value` fields contain expressions like `${{ jobs.build.outputs.version }}`.
pub fn resolve_outputs(
  defined: &HashMap<String, OutputDef>,
  job_outputs: &HashMap<String, HashMap<String, String>>,
) -> HashMap<String, String> {
  defined
    .iter()
    .map(|(name, def)| {
      let value = resolve_output_expression(&def.value, job_outputs);
      (name.clone(), value)
    })
    .collect()
}

/// Resolve a simple output expression like `${{ jobs.build.outputs.version }}`.
fn resolve_output_expression(
  expr: &str,
  job_outputs: &HashMap<String, HashMap<String, String>>,
) -> String {
  let trimmed = expr.trim();

  let inner = trimmed
    .strip_prefix("${{")
    .and_then(|s| s.strip_suffix("}}"))
    .map(str::trim);

  let Some(inner) = inner else {
    return trimmed.to_owned();
  };

  let parts: Vec<&str> = inner.split('.').collect();
  // Expected: ["jobs", "<job_id>", "outputs", "<output_name>"]
  if parts.len() == 4
    && parts.first().is_some_and(|p| *p == "jobs")
    && parts.get(2).is_some_and(|p| *p == "outputs")
  {
    let job_id = parts.get(1).copied().unwrap_or_default();
    let output_name = parts.get(3).copied().unwrap_or_default();

    return job_outputs
      .get(job_id)
      .and_then(|outputs| outputs.get(output_name))
      .cloned()
      .unwrap_or_default();
  }

  String::new()
}

/// Build the caller context for a reusable workflow invocation.
pub fn build_caller_context(
  inputs: &HashMap<String, String>,
  secrets: &HashMap<String, String>,
) -> CallerContext {
  CallerContext {
    inputs: inputs.clone(),
    secrets: secrets.clone(),
  }
}
