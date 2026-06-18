//! Step environment resolution and file-command result application.

use std::collections::HashMap;

use shared::{ActionStep, RunnerError};

use super::context::ExecutionContext;
use super::file_commands::FileCommandManager;

/// Extract step-level environment from the `environment` token.
pub(super) fn resolve_step_env(
  step: &ActionStep,
  ctx: &ExecutionContext,
) -> Result<HashMap<String, String>, RunnerError> {
  let Some(env_token) = &step.environment else {
    return Ok(HashMap::new());
  };
  let mut result = HashMap::new();
  let entries = env_token.d.as_deref().unwrap_or_default();
  for entry in entries {
    let Some(key) = entry.key.to_string_value() else {
      continue;
    };
    let raw = entry.value.to_string_value().unwrap_or_default();
    let value = ctx.interpolate_string(raw)?;
    result.insert(key.to_owned(), value);
  }
  Ok(result)
}

/// Process file commands after step execution; returns GITHUB_OUTPUT values.
pub(super) fn apply_file_commands(
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> HashMap<String, String> {
  let Ok(results) = file_cmds.process() else {
    tracing::warn!("failed to process file commands; step outputs/env will be empty");
    return HashMap::new();
  };
  for (key, value) in results.env_vars {
    ctx.set_env(&key, &value);
  }
  for dir in results.path_additions {
    ctx.prepend_path(&dir);
  }
  results.outputs
}
