//! Step environment resolution and file-command result application.

use std::collections::HashMap;

use shared::{ActionStep, RunnerError, TemplateToken};

use super::context::ExecutionContext;
use super::file_commands::FileCommandManager;
use expressions::evaluator::EvalContext;
use expressions::types::ExprValue;

/// Extract step-level environment from the `environment` token, evaluating
/// every entry against the caller's single per-step `eval_ctx` snapshot
/// (S4) instead of a fresh one per entry.
pub(super) fn resolve_step_env(
  step: &ActionStep,
  ctx: &ExecutionContext,
  eval_ctx: &EvalContext,
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
    let value = env_token_to_string(&entry.value, ctx, eval_ctx)?;
    result.insert(key.to_owned(), value);
  }
  Ok(result)
}

/// Render a scalar env-value token to its final string.
///
/// GitHub serializes `KEY: ${{ expr }}` as an expression token (type 3),
/// not a literal — reading only `to_string_value()` silently turned every
/// such value into `""` (live bug: `WHO=${{ inputs.who }}` came out
/// empty). Literals still pass through `interpolate_with` so an inline
/// `${{ }}` inside a literal keeps working; expressions are evaluated
/// with GitHub's string coercion. Bare scalars follow the same coercion
/// rules: booleans (type 5) render lowercase, numbers (type 6) drop a
/// trailing `.0`, and null (type 7) is the empty string.
fn env_token_to_string(
  token: &TemplateToken,
  ctx: &ExecutionContext,
  eval_ctx: &EvalContext,
) -> Result<String, RunnerError> {
  match token.token_type {
    0 => ctx.interpolate_with(eval_ctx, token.lit.as_deref().unwrap_or_default()),
    3 => {
      let expr = token.expr.as_deref().unwrap_or_default();
      Ok(ctx.evaluate_with(eval_ctx, expr)?.coerce_to_string())
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

/// Process file commands after step execution; returns `GITHUB_OUTPUT`
/// values. Does NOT record them on `ctx` — the caller
/// ([`apply_file_commands_and_merge_outputs`]) is the single writer, so a
/// `$GITHUB_OUTPUT` value can't be recorded once here and again by a caller's
/// own loop.
async fn apply_file_commands(
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> HashMap<String, String> {
  let Ok(results) = file_cmds.process().await else {
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

/// Apply a step's `$GITHUB_ENV`/`$GITHUB_PATH`/`$GITHUB_OUTPUT` file commands
/// AND merge them with its stdout-emitted (`::set-output::`) outputs into one
/// map, recording every output on `ctx` under `step_id` in a single pass.
///
/// `$GITHUB_OUTPUT` file-command outputs are merged first, then stdout
/// outputs overlay them via [`HashMap::extend`] (which replaces an existing
/// key) — a stdout `set-output` for a key the step also wrote to
/// `$GITHUB_OUTPUT` wins, matching the real runner's last-writer-wins
/// semantics. The merged map is then recorded on `ctx` exactly once; writing
/// the file-command and stdout maps to `ctx` as two separate loops (the prior
/// shape) let the file-command loop transiently overwrite a stdout value
/// already on `ctx`, which the stdout loop then had to correct back —
/// correct in the end, but two writes per overlapping key through a
/// window of the wrong value. Shared by the `run:` step path
/// ([`super::steps_runner`]) and Node.js action stages ([`super::node_stage`])
/// so both get identical env/PATH/output propagation.
pub(super) async fn apply_file_commands_and_merge_outputs(
  step_id: &str,
  stdout_outputs: HashMap<String, String>,
  file_cmds: &FileCommandManager,
  ctx: &mut ExecutionContext,
) -> HashMap<String, String> {
  let mut outputs = apply_file_commands(file_cmds, ctx).await;
  outputs.extend(stdout_outputs);
  for (key, value) in &outputs {
    ctx.set_step_output(step_id, key, value);
  }
  outputs
}
