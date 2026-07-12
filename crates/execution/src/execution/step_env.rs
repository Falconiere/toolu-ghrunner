//! Step environment resolution and file-command result application.

use std::collections::HashMap;

use shared::{ActionStep, RunnerError, TemplateToken};

use super::context::ExecutionContext;
use super::file_commands::FileCommandManager;
use expressions::types::ExprValue;

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
    let value = env_token_to_string(&entry.value, ctx)?;
    result.insert(key.to_owned(), value);
  }
  Ok(result)
}

/// Render a scalar env-value token to its final string.
///
/// GitHub serializes `KEY: ${{ expr }}` as an expression token (type 3),
/// not a literal — reading only `to_string_value()` silently turned every
/// such value into `""` (live bug: `WHO=${{ inputs.who }}` came out
/// empty). Literals still pass through `interpolate_string` so an inline
/// `${{ }}` inside a literal keeps working; expressions are evaluated
/// with GitHub's string coercion. Bare scalars follow the same coercion
/// rules: booleans (type 5) render lowercase, numbers (type 6) drop a
/// trailing `.0`, and null (type 7) is the empty string.
fn env_token_to_string(
  token: &TemplateToken,
  ctx: &ExecutionContext,
) -> Result<String, RunnerError> {
  match token.token_type {
    0 => ctx.interpolate_string(token.lit.as_deref().unwrap_or_default()),
    3 => {
      let expr = token.expr.as_deref().unwrap_or_default();
      Ok(ctx.evaluate_expression(expr)?.coerce_to_string())
    },
    5 => Ok(ExprValue::Bool(token.bool_val.unwrap_or_default()).coerce_to_string()),
    6 => Ok(ExprValue::Number(token.num_val.unwrap_or_default()).coerce_to_string()),
    7 => Ok(String::new()),
    other => {
      tracing::warn!(
        token_type = other,
        "unsupported step env value token type — using empty string"
      );
      Ok(String::new())
    },
  }
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
